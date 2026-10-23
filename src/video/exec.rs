//! 执行器后端（`[schedule] backend`）：nano 单核泵 + dynamic 多核 steal 泵。
//!
//! - `nano_pump`：一个永不结束的 pump task（取帧 → `upload_frame` →
//!   `pace_or_drop`，每帧 `Yield` 一次）。`render`/影子/vsync 全 honoring
//!   （`upload_frame` 内部分发），`workers`/`dist` 忽略。行为对标
//!   `blt_loop`/`direct_loop`（同样无 OSD）。
//! - `dynamic_draw`：按核分带，BSP 每帧 spawn，AP steal；BSP 等 done==N 后
//!   OSD + 限速/掉帧独判（确定性与手写一致：AP 无判帧权，只有 BSP 的结论算数）。
//!   只直写前台：`render=blt`（Blt 引擎单线程）/ 无 MP / 单核回 nano 泵；
//!   `tune.buffers>0`（影子环 v1 不支持）回 manual（影子整推只在手写路）。
//!   数据面经 `SlotRing` 槽环（steal task 解码进槽 `Full` 发布，BSP 按
//!   `next_full_min_gen` 消费最低世代槽、整推后 `release_empty`；固定槽-带
//!   映射，帧号 sidecar 防 stale 上传）；控制面（分发/steal）仍归执行器。
//!   新标记行（矩阵 follow-up 断言用）：
//!   `play: dynamic slots armed bands=<n> slots=<n>`（建环时记一笔）
//!   + 原有 `play: path=dynamic_draw (...)` 行逐字保留。
//!
//! 共享纪律（沿用 `mp_draw` 风格，会话期常驻，播完不回收）：
//!
//! 等待纪律（看门狗铁律）：BSP 的 done 等待与槽消费等待一律 tsc-bounded
//!  （5·tps 熔断；超时按丢帧/缺带降级，不断流）；steal task 的槽认领同样
//!   bounded（失配 stale 全槽由 BSP 开帧清理，见 `BSPDriver::run`）。
//!
//! 统一控制面：`nano_pump` 全档供片直通（含 `single` 零缓存重解 + `stream` 窗预取，
//!   取帧→上传→限速/掉帧，BSP 单泵）；`dynamic_draw` 恒 `eager`（需全前视分带，load 期
//!   已强制，AP 只 steal 行拷、永不碰 GOP）。解码 SIMD 单比特跟随全局。

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use uefi::boot::{EventType, Tpl, create_event, get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::pi::mp::MpServices;

use uefi_async::Yield;
use uefi_async::dynamic::Executor as DynExec;
use uefi_async::nano_alloc::{Executor as NanoExec, TaskNode as NanoNode};
use uefi_async::tick;

use crate::config::{self, CopyWidth};
use crate::error::{NyaStatus, Result};
use crate::font::latin::FONT_8X16;
use crate::graphics::{Screen, copy_block};
use crate::video::decoder::Clip;
use crate::video::sync::{SlotRing, SlotState};
use crate::video::{pace_or_drop, rdtsc};

/// OSD 顶条行数（与手写路 `OSD_ROWS` 同值；steal 分带恒跳过，AP 不盖字）。
const OSD_ROWS: usize = crate::video::OSD_ROWS;

/// 位图字：位 1 画 color，位 0 画黑（与手写路 `draw_string_opaque` 同像素语义）。
fn draw_str_raw(fb_base: *mut u32, stride: usize, x: u32, y: u32, s: &[u8], color: u32) {
    let mut current_x = x;
    for &char_code in s {
        let glyph = FONT_8X16[(char_code & 0x7F) as usize];
        for row in 0..16 {
            let row_data = glyph[row];
            unsafe {
                let row_ptr = fb_base.add(((y + row as u32) * stride as u32 + current_x) as usize);
                for col in 0..8 {
                    let pixel_color = if (row_data << col) & 0x80 != 0 {
                        color
                    } else {
                        0x000000
                    };
                    row_ptr.add(col).write_volatile(pixel_color);
                }
            }
        }
        current_x += 8;
    }
}

/// 单核执行器泵：`blt_loop`/`direct_loop` 的执行器形态（单 task、无 OSD）。
/// `[display]`：起播记一笔 `display:` marker（源取 `Clip::w/h`）；上屏同取
/// clip 几何（形参旧 `VIDEO_W/H` 常量，1080p 恒等，小尺寸以 clip 为准）。
pub fn nano_pump(
    screen: &mut Screen,
    mut clip: Clip,
    _width: usize,
    _height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    {
        let (dw, dh) = screen.get_gop().current_mode_info().resolution();
        crate::graphics::present::note_clip_start(
            clip.w as u32,
            clip.h as u32,
            dw as u32,
            dh as u32,
        );
    }
    let (sw, sh) = (clip.w, clip.h);
    let mut drops = 0u64;
    let err: Rc<RefCell<Option<NyaStatus>>> = Rc::new(RefCell::new(None));
    let eflag = Rc::clone(&err);
    // 单 task 包一切：`&mut` 全在自己手里，无跨任务共享，无需原子量。
    let pump = async move {
        loop {
            let fstart = rdtsc();
            match clip.next_frame() {
                Some(frame) => {
                    if let Err(e) = screen.upload_frame(sw, sh, frame) {
                        *eflag.borrow_mut() = Some(e);
                        break;
                    }
                }
                None => clip.rewind(),
            }
            pace_or_drop(&mut clip, target, tps, fstart, &mut drops);
            // 协作点：让出 poll，一帧一调度（否则首 poll 就卡死在无限循环里）。
            Yield.await;
        }
    };
    let mut exec = NanoExec::new();
    let mut node = NanoNode::new(Box::pin(pump), 0);
    exec.add(&mut node);
    let mut cx = NanoExec::init_step();
    loop {
        exec.run_step(tick(), &mut cx);
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
    }
}

/// AP 上下文：执行器数组基址 + MP 协议（`who_am_i` 取号）。
/// 裸指针纪律：每台执行器只被一核 poll（BSP 固定 0 号，AP 按号取）。
struct ApCtx {
    mp: *const MpServices,
    execs: *const DynExec<'static>,
    n: usize,
}

extern "efiapi" fn exec_ap(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const ApCtx) };
    let my_id = unsafe { (*ctx.mp).who_am_i().unwrap_or(usize::MAX) };
    if my_id == 0 {
        return;
    }
    // 选中集门（`crew::ap_index_in_crew`）：编号 ≥ k 的闲 AP 早退（见 `crew`
    // 模块头 start-all-but-idle-unchosen），不 poll 执行器、不 steal。
    if config::ap_index_in_crew(my_id, ctx.n).is_none() {
        return;
    }
    let exec = unsafe { &*ctx.execs.add(my_id) };
    let mut cx = DynExec::init_step();
    loop {
        exec.run_step(tick(), &mut cx);
    }
}

/// BSP 驱动状态（自持：执行器阵列 + MP 句柄 + 帧表，帧内不返回即常驻）。
struct BSPDriver {
    exec0: *const DynExec<'static>,
    done: &'static AtomicUsize,
    addrs: &'static [*const u8],
    fb_base: usize,
    stride_bytes: usize,
    scr_h: usize,
    nbands: usize,
    per: usize,
    /// 槽环（`[nbands]`，槽 b 只装带 b 的行字节；steal task 产出，BSP 消费），
    /// + 帧号 sidecar（`[nbands]`，task 存 fi 后 `publish_full`，BSP 验号防 stale）。
    ring: &'static SlotRing<32>,
    seqs: &'static [AtomicUsize],
    copy_w: CopyWidth,
    target: Option<u32>,
    tps: u64,
    total: usize,
}

impl BSPDriver {
    /// 分带 spawn（steal task 解码进槽）→ BSP 消费槽整推 → OSD → 限速/掉帧独判。
    /// 等待一律 tsc-bounded（5·tps 熔断）：done 超时则按已有槽降级消费，
    /// 槽超时则记缺带 drop；stale 槽（上一轮超时残留）开帧清理 + 消费时验号。
    fn run(&self) -> Result {
        // SAFETY：0 号执行器只被 BSP poll（AP 按号取自己的，见 `exec_ap`）。
        let exec0 = unsafe { &*self.exec0 };
        let mut cx = DynExec::init_step();
        let mut idx = 0usize;
        let mut iter = 0usize;
        let mut drops = 0u64;
        let mut next_start = rdtsc();
        // OSD 计时（手写路口径：64 帧一刷 FPS/FT/Margin/Drop）。
        let mut start_ticks = rdtsc();
        let mut last_sample = start_ticks;
        let mut fps_counter = 0u32;
        let stride_px = self.stride_bytes / 4;
        // 120Hz 视频假设与手写路一致（`VIDEO_NATIVE_FPS`）。
        let target_ticks_per_frame = self.tps as u128 / 120;
        loop {
            // 1. 限速到槽 + 迟到判丢（max 模式直接过；BSP 独判，无世代协议）。
            if let Some(fps) = self.target {
                let budget = self.tps.max(1) / fps.max(1) as u64;
                while rdtsc() < next_start {
                    core::hint::spin_loop();
                }
                let fstart = rdtsc();
                if fstart.saturating_sub(next_start) > budget.saturating_mul(2) {
                    idx = (idx + 1) % self.total.max(1);
                    drops += 1;
                }
                next_start = fstart + budget;
            }
            let fi = idx % self.total.max(1);
            let src = self.addrs[fi] as usize;
            // 开帧清理：上一轮 done 超时残留的 stale Full（帧号 != fi）直接
            // 认领释放，免得本轮 task 认领失败（单消费 BSP，无竞争）。
            for b in 0..self.nbands {
                if let Some(slot) = self.ring.get(b) {
                    if slot.state() == SlotState::Full
                        && self.seqs[b].load(Ordering::Acquire) != fi
                        && slot.try_acquire_drain()
                    {
                        slot.release_empty();
                    }
                }
            }
            // 2. 分带 spawn（空带跳过；steal task 把行带拷进槽 b 后 Full 发布，
            //    槽认领同样 bounded——失配只发生在 stale 清理漏网时，band 缺席
            //    由 BSP 消费侧记 miss，不断流）。
            let mut n = 0usize;
            for b in 0..self.nbands {
                let y0 = OSD_ROWS + b * self.per;
                let y1 = (y0 + self.per).min(self.scr_h);
                if y0 >= y1 {
                    continue;
                }
                let done = self.done;
                let (ring, seqs) = (self.ring, self.seqs);
                let (src, stride) = (src, self.stride_bytes);
                let task = async move {
                    // 固定槽-带映射：槽 b 只装带 b（BSP 消费侧验帧号防 stale）。
                    if let Some(slot) = ring.get(b) {
                        if slot.try_acquire_fill() {
                            unsafe {
                                let dst = slot.as_bytes_mut();
                                let sbase = src as *const u8;
                                for (r, y) in (y0..y1).enumerate() {
                                    let d = dst.as_mut_ptr().add(r * stride);
                                    let s = sbase.add(y * stride);
                                    core::ptr::copy_nonoverlapping(s, d, stride);
                                }
                            }
                            seqs[b].store(fi, Ordering::Release);
                            slot.publish_full();
                        }
                    }
                    done.fetch_add(1, Ordering::SeqCst);
                };
                exec0.spawn(Box::pin(task), 0).ok_or_else(|| {
                    NyaStatus::_Debug(alloc::string::String::from(
                        "play: dynamic spawn queue full",
                    ))
                })?;
                n += 1;
            }
            // 3. 等 done==n（BSP 轮询 0 号执行器，顺手执行自己分到的带；
            //    tsc-bounded：5·tps 到界即降级消费已有槽，缺带记 miss，不断流）。
            {
                let deadline = rdtsc().saturating_add(5 * self.tps.max(1));
                while self.done.load(Ordering::Acquire) < n {
                    exec0.run_step(tick(), &mut cx);
                    if rdtsc() >= deadline {
                        break;
                    }
                }
                self.done.store(0, Ordering::SeqCst);
            }
            // 3.5 BSP 消费槽 → 整推（最低世代优先；验帧号防 stale，失配释放；
            //    缺槽/超时按缺带记 drops，与迟到判丢同口径，BSP 独判）。
            {
                let deadline = rdtsc().saturating_add(5 * self.tps.max(1));
                let mut got = 0usize;
                for _ in 0..n {
                    let mut idx_opt = None;
                    while rdtsc() < deadline {
                        idx_opt = self.ring.next_full_min_gen();
                        if idx_opt.is_some() {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                    let Some(b) = idx_opt else { break };
                    let slot = match self.ring.get(b) {
                        Some(s) => s,
                        None => continue,
                    };
                    if !slot.try_acquire_drain() {
                        continue;
                    }
                    if self.seqs[b].load(Ordering::Acquire) != fi {
                        // stale（上一轮超时残留，task 已重产）：释放，不上传。
                        slot.release_empty();
                        continue;
                    }
                    let y0 = OSD_ROWS + b * self.per;
                    let y1 = (y0 + self.per).min(self.scr_h);
                    unsafe {
                        let bytes = slot.as_bytes();
                        for (r, y) in (y0..y1).enumerate() {
                            let d = (self.fb_base as *mut u8).add(y * self.stride_bytes);
                            let s = bytes.as_ptr().add(r * self.stride_bytes);
                            copy_block(d, s, self.stride_bytes, self.copy_w);
                        }
                    }
                    slot.release_empty();
                    got += 1;
                }
                drops += (n as u64).saturating_sub(got as u64);
            }
            // 4. OSD（BSP 独画，AP 行区已跳过顶条）。
            {
                let end_ticks = rdtsc();
                let delta = end_ticks.saturating_sub(start_ticks) as u128;
                let ft_us = delta * 1_000_000 / self.tps.max(1) as u128;
                let margin_us = if target_ticks_per_frame > delta {
                    (target_ticks_per_frame - delta) * 1_000_000 / self.tps.max(1) as u128
                } else {
                    0
                };
                fps_counter += 1;
                if (fps_counter & 63) == 0 {
                    let span = end_ticks.saturating_sub(last_sample) as u128;
                    let fps = if span > 0 {
                        (64 * self.tps as u128) / span
                    } else {
                        0
                    };
                    last_sample = end_ticks;
                    let fb = self.fb_base as *mut u32;
                    draw_str_raw(
                        fb,
                        stride_px,
                        0,
                        0,
                        format!("FPS: {:>4}", fps).as_bytes(),
                        0x00FF00,
                    );
                    draw_str_raw(
                        fb,
                        stride_px,
                        200,
                        0,
                        format!("FT: {:>5} us", ft_us).as_bytes(),
                        0x00FFFF,
                    );
                    draw_str_raw(
                        fb,
                        stride_px,
                        450,
                        0,
                        format!("Margin: {:>5} us Drop:{}", margin_us, drops).as_bytes(),
                        0xFFA500,
                    );
                }
                start_ticks = end_ticks;
            }
            idx = (idx + 1) % self.total.max(1);
            iter += 1;
            // 里程碑：BSP 每 256 轮报一次（串口-only；卡死时最后行号即定位）。
            if (iter & 255) == 0 {
                log::warn!("play: alive round={} drops={}", iter, drops);
            }
        }
    }
}

/// 多核 steal 泵：BSP 分带 spawn，AP steal；BSP 独判限速/掉帧/OSD。
/// `[display]` v1 视觉不变（仍 native 左上 + 黑垫，见 `mp.rs` 头同因：分带按屏
/// 行切，缩放需 BSP 起播预处理整帧表，另立项；marker 照记配置意图）。
pub fn dynamic_draw(
    screen: &mut Screen,
    clip: Clip,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    // --- 回退门（warn + 走语义最接近的现货路，不断播） ---
    if config::effective_render() == config::RenderMode::Blt {
        log::warn!(
            "play: dynamic needs direct writes (Blt engine is single-threaded), fallback nano_pump"
        );
        return nano_pump(screen, clip, width, height, target, tps);
    }
    if config::tune_buffers() > 0 {
        log::warn!("play: dynamic has no shadow ring in v1, fallback manual");
        return super::manual_run(screen, clip, target, tps);
    }
    let mp_handle = match get_handle_for_protocol::<MpServices>() {
        Ok(h) => h,
        Err(_) => {
            log::warn!("play: dynamic needs MP Services, fallback nano_pump");
            return nano_pump(screen, clip, width, height, target, tps);
        }
    };
    let mp = open_protocol_exclusive::<MpServices>(mp_handle)?;
    // 选中集（`crew` 决议：BSP-id 自检 + 预算/记忆/workers 优先级；分带/槽环
    // 全按 k 建，闲 AP 由 `exec_ap` 入口早退，仍走单次 broadcast，见 `crew` 头）。
    let detected = mp.get_number_of_processors()?.enabled.max(1);
    let n_cores = config::resolve_crew_count(detected, mp.who_am_i().unwrap_or(0));
    // dynamic 后端恒 steal 泵（不分行/槽角色）：sched 行照打，roles 仅信息位
    // （显式 split 不准入时同 manual 路 warn 一次）。
    config::emit_sched_line(n_cores, detected, config::effective_roles(n_cores));
    if n_cores <= 1 {
        log::warn!("play: dynamic on single core, fallback nano_pump");
        return nano_pump(screen, clip, width, height, target, tps);
    }
    log::info!("play: path=dynamic_draw ({} cores, steal bands)", n_cores);
    // --- 几何 + 分带 + 槽环后备（`clip` 尚未 drop，槽分配失败可回 nano 泵） ---
    // 分带 `[OSD_ROWS, scr_h)` 等分（末带兜余行）；32 上限对齐 `SlotRing<32>`
    // （`sync` 断言 N≤32；>32 核由 steal 分摊，行为见文件头）。
    let (scr_w, scr_h) = screen.get_gop().current_mode_info().resolution();
    let (scr_w, scr_h) = (scr_w as usize, scr_h as usize);
    {
        crate::graphics::present::note_clip_start(
            clip.w as u32,
            clip.h as u32,
            scr_w as u32,
            scr_h as u32,
        );
    }
    let stride_bytes = screen.get_gop().current_mode_info().stride() * 4;
    let nbands = n_cores.min(32).max(1);
    let per = scr_h.saturating_sub(OSD_ROWS).div_ceil(nbands).max(1);
    // 槽环后备（`nbands` 槽 × 每带 `per` 行；steal task 产带字节，BSP 消费整推，
    // 会话期常驻，播完不回收）。
    let slot_len = per.saturating_mul(stride_bytes).max(1);
    let mut backing: Vec<u8> = Vec::new();
    if backing.try_reserve_exact(nbands * slot_len).is_err() {
        log::warn!("play: dynamic slots alloc failed, fallback nano_pump");
        return nano_pump(screen, clip, width, height, target, tps);
    }
    backing.extend(core::iter::repeat(0u8).take(nbands * slot_len));
    let backing_leaked: &'static mut [u8] = Box::leak(backing.into_boxed_slice());
    let ring: &'static SlotRing<32> = Box::leak(Box::new(unsafe {
        SlotRing::new(backing_leaked.as_mut_ptr(), slot_len, nbands)
    }));
    let mut seqv: Vec<AtomicUsize> = Vec::new();
    for _ in 0..nbands {
        seqv.push(AtomicUsize::new(usize::MAX));
    }
    let seqs: &'static [AtomicUsize] = Box::leak(seqv.into_boxed_slice());
    log::info!(
        "play: dynamic slots armed bands={} slots={}",
        nbands,
        nbands
    );
    let (vw, vh) = (clip.w, clip.h);
    let row_bytes = vw * 4;
    let mut full: Vec<Vec<u8>> = Vec::new();
    for frame in &clip.frames {
        let raw: &[u8] =
            unsafe { core::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) };
        if raw.len() < vh.saturating_mul(row_bytes) {
            continue;
        }
        let mut f: Vec<u8> = alloc::vec![0u8; scr_h * stride_bytes];
        for y in 0..scr_h {
            if y < vh {
                f[y * stride_bytes..y * stride_bytes + row_bytes]
                    .copy_from_slice(&raw[y * row_bytes..(y + 1) * row_bytes]);
            }
        }
        full.push(f);
    }
    if full.is_empty() {
        return Err(NyaStatus::_Debug(alloc::string::String::from(
            "video: no valid frames",
        )));
    }
    // `clip` 至此功成身退（整表已拷出）：显式 drop 省一半常驻内存。
    let total = full.len();
    drop(clip);
    // 会话期常驻：帧内存 + 地址表 leak（播完不回收，沿用 `mp_draw` 风格）。
    let full_leaked: &'static mut [Vec<u8>] = Box::leak(full.into_boxed_slice());
    let addrs: Vec<*const u8> = full_leaked.iter().map(|f| f.as_ptr()).collect();
    let addrs_leaked: &'static [*const u8] = Box::leak(addrs.into_boxed_slice());
    let _ = (scr_w, full_leaked);

    // --- 执行器阵列：一核一台，stealer 全互联（BSP 建完再起 AP） ---
    let mut execs: Vec<DynExec<'static>> = (0..n_cores).map(|_| DynExec::new()).collect();
    {
        let stealers: Vec<_> = execs.iter().map(|e| e.stealer()).collect();
        for (i, e) in execs.iter_mut().enumerate() {
            for (j, s) in stealers.iter().enumerate() {
                if i != j {
                    e.add_stealer(s.clone());
                }
            }
        }
    }
    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr() as usize;
    let done: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    // 分带几何沿用上文（`nbands`/`per` 已按 `SlotRing<32>` 上限裁定）。

    // --- 起 AP（BSP 不进 AP 函数，0 号执行器归 BSP 轮询） ---
    let event = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None)? };
    // `execs`/`mp`/`full` 常驻本栈（帧内不返回）：AP 经裸指针取用，安全。
    // `Box::leak` 显式化 AP 上下文（24 字节，会话期常驻）。
    let mp_ref: &MpServices = &mp;
    let apctx = Box::leak(Box::new(ApCtx {
        mp: mp_ref as *const MpServices,
        execs: execs.as_ptr(),
        n: n_cores,
    }));
    if let Err(e) = mp.startup_all_aps(
        false,
        exec_ap,
        apctx as *mut ApCtx as *mut c_void,
        Some(event),
        None,
    ) {
        log::warn!(
            "play: startup_all_aps failed: {:?}, BSP drains bands alone",
            e.status()
        );
    }
    BSPDriver {
        exec0: &execs[0] as *const DynExec<'static>,
        done,
        addrs: addrs_leaked,
        fb_base,
        stride_bytes,
        scr_h,
        nbands,
        per,
        ring,
        seqs,
        copy_w: config::copy_width(),
        target,
        tps,
        total,
    }
    .run()
}
