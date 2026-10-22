//! `video::mp::mp_slots`：qois/raw 真·逐核解码帧分区槽环路（stage-2 接线）。
//!
//! 轮子结论：锁/环原语用 `super::sync` 自研（`crossbeam` 等要 `std` 线程，
//! UEFI 无线程无 futex，`no_std + alloc only` 不许进 `std` 轮子，
//! 见 `AGENTS.md`“没有才造”；调度仍归 `MpServices` + 下述手写 rendezvous，
//! 本模块只做数据面，不管任务分发）。
//!
//! # 接入条件（`use_frame_slots`，分发前 `mp_draw` 裁决；`need_eager_clip` 联动）
//! - 片源 qois/raw：帧独立、无跨帧解码状态（可行性见 `decoder::source` 文件头；
//!   有状态六格式恒走行分区 eager 路）；
//! - 选中核数 > 1（`workers` 已折进选中集：bsp_only/预算/记忆全走到 `crew` 数，
//!   不另查 `effective_workers` 三态）；
//! - 有效供片（`config::effective_feed(fmt)`，每格式 `fetch` 覆盖全局后）为
//!   `zero_copy` / `single` / `stream`（`clip`/`staged` 走旧路，逐字节旧行为）；
//! - `roles != combined`（显式合恒行路；显式分不准入时 warn + 回落，见
//!   `config::effective_roles`）；
//! - `Clip` 为 lazy 形态（`need_eager_clip` 已为本格返回 false，编码源保留）。
//! 其余一律回行分区 eager 路（`mp_draw` 下半段，不动；lazy 残留就地 `ensure_eager`）。
//!
//! # 协议图（真并行：BSP 只 scan-split，AP 各解各的帧，BSP 按序收槽上屏）
//! ```text
//! BSP setup：读片源 → scan（qois 用 lazy::scan_qois_chunks 本体 / raw 用
//!   lazy::list_raw_names 本体，不重写拆帧）→ 连续切 N 块 → 常驻泄漏
//! AP c 认领块内 fi → 索引解码（qois: decode_qois_chunk 纯函数 / raw:
//!   decode_raw_frame 读盘+reinterpret）→ pack 进自家环槽 → seq[slot]=fi → publish_full
//! BSP 按 fi 递增 → owner(fi) 环里找 Full 且 seq==fi → try_acquire_drain → 整推 → release_empty
//! ```
//! - 分块连续（核 c 拿 `[c*T/N, (c+1)*T/N)`，`owner(fi)=((fi+1)*N-1)/T`），
//!   每帧恰一 owner；AP 块内顺序解码、循环重播，与 BSP 消费顺序一致。
//! - qois 解码纯函数（块字节 + 几何 → BGRA，无会话无跨块状态，N 核零竞争）；
//!   口径与 `source::qoi_into` 同（超大帧拒绝 + 几何一致 + R/B 交换），
//!   坏块 AP 跳过（BSP 5·tps 熔断记 drop，见下）。
//! - raw 零解码：`frameNNNN.raw` 即 BGRA 直存，`bytes_as_pixels` reinterpret
//!   就是解码（零像素换算；字节所有权转像素 Vec，读后不常驻）。
//!   槽里放的是行区拷贝（`pack_frame`，stride 对齐 + 补零 + 超视频行置零），
//!   源字节/像素缓冲为 AP 私有复用（qois scratch 常驻复用，raw 逐帧接管）。
//! - raw 读盘与 `Source::Raw` 臂同式（`Fs::new` 现开现读；MP 并发读盘以后固件
//!   实测为准，BSP 侧 bounded 等待兜底——AP 卡死只丢该核帧，播放不断流）。
//! - BSP 独判掉帧（P3 确定性：`target_fps` 预算超 2 倍 → 下轮 skip=1；
//!   AP 只管按序产帧，不判帧）；BSP 超时等不到槽（5·tps 熔断）即记 drop 跳过。
//! - AP 永远只碰自家环槽 + 私有解码缓冲 + 只读共享编码源，永不碰 GOP/Blt/前台
//!   framebuffer；整推（CPU 直写/shadow Blt）只 BSP（旧纪律不变）。
//! - AP 退场：自家两槽 30·tps 无空位 → AP 直接返回（BSP 侧 bounded 等待会
//!   把该核帧逐个 drop，播放不卡死；正常机永不到此）。
//!
//! # 等待纪律（固件看门狗铁律，永不无界自旋）
//! - BSP 找槽：截止期 5·tps（`MpDrive::wait_done` 同款 5 秒熔断）；
//! - AP 等空槽：截止期 30·tps（BSP 活着则恒有空位，属进度保证而非死等）；
//! - 限速自旋沿用 eager 口径（预算内 `spin_loop`，预算外判迟；max 不限速跳过）。
//!
//! # 新串口标记行（矩阵 follow-up 断言用，不断言画质；旧行逐字保留）
//! - `play: path=mp_slots frame-parallel fmt=<qois|raw> fetch=<feed> cores=<n> slots=<n>`
//!   （分发成功记一笔，每会话一次；本标记串不变，真并行已在内）；
//! - `mp: slots scan qois chunks=<t> <w>x<h>` / `mp: slots scan raw frames=<t> 1920x1080`
//!   （BSP scan-split 记一笔，每会话一次；qois 几何取首个可解块）；
//! - `play: alive round=<r> drops=<d>`（BSP 每 256 轮，与 eager 同串）；
//! - `video: no valid frames`（空集报错，与 eager 同串）。
//!
//! # unsafe 政策
//! - 裸指针只在持有者独占期解引用（槽缓冲经 `as_bytes_mut` 独写期、BSP
//!   `Draining` 独读期；编码源/环/seq 会话期 `Box::leak` 常驻，播完不回收，
//!   同 `mp_draw` 既有风格）；`unsafe fn` 体内显式 `unsafe {}` 块（edition 2024）。
//!
//! # `[display]` v1 视觉不变（仍 native 左上）：AP 按序号真解码自家块，缩放需
//! 每 AP 一份屏暂存 + 逐帧 CPU 成本，另立项；`display:` marker 由 `mp_draw`
//! 入口统一记一笔（本模块不再重复）。

use super::OSD_ROWS;
use crate::config::{self, CopyWidth, MediaFormat, RolesMode};
use crate::error::Result;
use crate::fs::Fs;
use crate::graphics::{Screen, copy_block};
use crate::video::ascii_font::FONT_8X16;
use crate::video::decoder::{
    Clip, decode_qois_chunk, decode_raw_frame, list_raw_names, scan_qois_chunks,
};
use crate::video::rdtsc;
use crate::video::sync::{SlotRing, SlotState, tsc_now};
use crate::video::{VIDEO_H, VIDEO_W};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};
use uefi::Status;
use uefi::boot::{EventType, Tpl, create_event};
use uefi::cstr16;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::pi::mp::MpServices;

/// 每核槽环深（1 在推 + 1 预取；总预取 ≤ 2·N 帧，内存有界）。
const SLOTS_PER_CORE: usize = 2;

/// 供片格由 `config::slots_feed_eligible` 统一口径（本模块不再自定，避免双源）。
/// 接入裁决（`mp_draw` 分发前调；`clip.lazy.is_some()` 另由调用方确认）：
/// 准入（供片格 + 选中核数 > 1）且 `roles != combined`（`auto` 准入即分；
/// 显式 `split` 不准入时 `effective_roles` 内 warn + 回落，本函数只取布尔）。
pub(crate) fn use_frame_slots(n_cores: usize) -> bool {
    config::slots_eligible(n_cores) && config::effective_roles(n_cores) != RolesMode::Combined
}

/// 编码源（BSP scan-split 产物，会话期常驻只读共享；AP 按序号索引解码）。
enum SlotSource {
    /// qois 整文件字节 + 纯扫描分块（`scan_qois_chunks` 本体）+ 首个可解块定几何。
    Qois {
        data: &'static [u8],
        ranges: &'static [Range<usize>],
        w: usize,
        h: usize,
        total: usize,
    },
    /// raw 目录名表（`list_raw_names` 本体）+ 定值几何（1920x1080，同预解码）。
    Raw {
        dir: &'static str,
        names: &'static [String],
        frame_bytes: usize,
        total: usize,
    },
}

impl SlotSource {
    fn total(&self) -> usize {
        match self {
            SlotSource::Qois { total, .. } | SlotSource::Raw { total, .. } => *total,
        }
    }

    /// 连续分块 owner：核 c 拿 `[c*T/N, (c+1)*T/N)`，每 fi 恰属一核。
    fn owner(&self, fi: usize, n: usize) -> usize {
        let t = self.total().max(1);
        ((fi.saturating_add(1).saturating_mul(n).saturating_sub(1)) / t).min(n.saturating_sub(1))
    }

    /// 核 c 的块范围（空块时 start >= end，AP 直接退场）。
    fn chunk(&self, c: usize, n: usize) -> (usize, usize) {
        let t = self.total().max(1);
        let s = c.saturating_mul(t) / n.max(1);
        let e = (c.saturating_add(1).saturating_mul(t) / n.max(1)).min(t);
        (s.min(t), e.min(t))
    }
}

/// 槽任务上下文（会话期 `Box::leak` 常驻；AP 只读编码源 + 独写自家环，
/// BSP 独推，裸指针纪律同 `PlayTask`）。
struct SlotTask<'a> {
    mp: &'a MpServices,
    fb_base: *mut u8,
    stride_bytes: usize,
    scr_h: usize,
    stride_row_bytes: usize,
    vid_h: usize,
    num_cores: usize,
    /// 编码源（BSP scan-split 产物；AP 索引解码只读）。
    source: SlotSource,
    /// 每核一环（`[num_cores]`，核 c 只碰 `rings[c]`）。
    rings: &'static [SlotRing<SLOTS_PER_CORE>],
    /// 帧号 sidecar（`[num_cores * SLOTS_PER_CORE]`，槽 s 属核 c 时下标 `c*2+s`）。
    seqs: *const AtomicUsize,
    copy: u8,
    target_fps: u32,
    tps: u64,
    /// 后缓冲影子槽（`tune.buffers > 0` 时；BSP 独写独推，AP 不碰，语义同 eager）。
    sh_slots: *const *mut u8,
    sh_n: usize,
    sh_row: usize,
    sh_h: usize,
    /// GOP 裸指针（只给 BSP 调 Blt 整推用；AP 永不碰 GOP）。
    gop: *mut GraphicsOutput,
}

/// 位图字（与 `mp::play_task` 内嵌版同像素语义：位 1 画 color，位 0 画黑）。
fn draw_string_opaque(fb_base: *mut u32, stride: usize, x: u32, y: u32, s: &[u8], color: u32) {
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

extern "efiapi" fn slot_task(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const SlotTask) };
    let my_id = unsafe { (*ctx.mp).who_am_i().unwrap_or(usize::MAX) };
    // 选中集门（`crew::ap_index_in_crew`）：编号 ≥ k 的闲 AP 早退（见 `crew`
    // 模块头 start-all-but-idle-unchosen），永不碰环槽/GOP。
    if config::ap_index_in_crew(my_id, ctx.num_cores).is_none() {
        return;
    }
    if my_id == 0 {
        bsp_loop(ctx);
    } else {
        ap_loop(ctx, my_id);
    }
}

/// 槽帧打包（stride 对齐全帧；视频行拷入 + stride 补零，超视频行置零；
/// OSD 行区不管——BSP 上传时跳过，与 dynamic 全帧表同语义）。
fn pack_frame(
    slot: &mut [u8],
    frame: &[u8],
    row_bytes: usize,
    vid_h: usize,
    stride: usize,
    scr_h: usize,
) {
    for y in 0..scr_h {
        let d = &mut slot[y * stride..(y + 1) * stride];
        if y < vid_h {
            let s = &frame[y * row_bytes..(y + 1) * row_bytes];
            let n = row_bytes.min(stride);
            d[..n].copy_from_slice(&s[..n]);
            for b in &mut d[n..] {
                *b = 0;
            }
        } else {
            for b in d.iter_mut() {
                *b = 0;
            }
        }
    }
}

/// AP 按序号解单块（真并行落点；`false` = 坏块/缺帧，调用方跳过发布）：
/// qois 走 `decode_qois_chunk` 纯函数（`scratch` 复用零稳态分配）；
/// raw 走 `decode_raw_frame`（读盘 + reinterpret 即解码，`scratch` 被接管替换）。
fn ap_decode(src: &SlotSource, fi: usize, scratch: &mut Vec<BltPixel>) -> bool {
    match src {
        SlotSource::Qois {
            data, ranges, w, h, ..
        } => {
            let r = match ranges.get(fi) {
                Some(r) => r.clone(),
                None => return false,
            };
            decode_qois_chunk(data, r, *w, *h, scratch)
        }
        SlotSource::Raw {
            dir,
            names,
            frame_bytes,
            ..
        } => {
            let name = match names.get(fi) {
                Some(n) => n,
                None => return false,
            };
            decode_raw_frame(dir, name, *frame_bytes, scratch)
        }
    }
}

/// AP 核循环：按序真解码自家块（连续分块，循环重播），满压即等、有界退场。
fn ap_loop(ctx: &SlotTask, my_id: usize) {
    let n = ctx.num_cores;
    let total = ctx.source.total();
    // 自家块（升序循环；BSP 按 fi 递增消费，顺序与此一致，见模块头）。
    let (start, end) = ctx.source.chunk(my_id, n);
    if start >= end || total == 0 {
        // 空块（帧比核少时的闲核）：直接退场（BSP 不依赖它）。
        return;
    }
    let ring = &ctx.rings[my_id];
    // 私有解码缓冲（qois 复用零稳态分配；raw 逐帧被接管替换，同 lazy 单槽语义）。
    let mut scratch: Vec<BltPixel> = Vec::new();
    let mut k = 0usize;
    loop {
        let fi = start + (k % (end - start));
        k = k.wrapping_add(1);
        if !ap_decode(&ctx.source, fi, &mut scratch) {
            // 坏块：跳过发布（BSP 侧 bounded 等待记 drop）；不占槽，直接下一帧。
            continue;
        }
        // 等自家空槽（BSP 活着则恒有空位；30·tps 到界即退场，不死等）。
        let deadline = tsc_now().saturating_add(30 * ctx.tps.max(1));
        loop {
            let mut filled = false;
            if let Some(s) = ring.next_empty() {
                let slot = ring.get(s).unwrap_or_else(|| unreachable!());
                if slot.try_acquire_fill() {
                    unsafe {
                        let dst = slot.as_bytes_mut();
                        let src = core::slice::from_raw_parts(
                            scratch.as_ptr() as *const u8,
                            scratch.len().saturating_mul(4),
                        );
                        pack_frame(
                            dst,
                            src,
                            ctx.stride_row_bytes,
                            ctx.vid_h,
                            ctx.stride_bytes,
                            ctx.scr_h,
                        );
                    }
                    unsafe {
                        (*ctx.seqs.add(my_id * SLOTS_PER_CORE + s)).store(fi, Ordering::Release)
                    };
                    slot.publish_full();
                    filled = true;
                }
            }
            if filled {
                break;
            }
            if tsc_now() >= deadline {
                return;
            }
            core::hint::spin_loop();
        }
    }
}

/// BSP 找槽：owner 环里 Full 且帧号 == fi 的槽（bounded，5·tps 熔断）。
/// 命中返槽下标（已 `try_acquire_drain`，调用方上传后 `release_empty`）；
/// 超时返 `None`（调用方记 drop 跳过）。
fn bsp_acquire_frame(ctx: &SlotTask, fi: usize) -> Option<usize> {
    let owner = ctx.source.owner(fi, ctx.num_cores.max(1));
    let ring = &ctx.rings[owner];
    let deadline = tsc_now().saturating_add(5 * ctx.tps.max(1));
    loop {
        for s in 0..SLOTS_PER_CORE {
            let slot = ring.get(s).unwrap_or_else(|| unreachable!());
            // Full 才读 seq（发布前已 `Release` 写，`Acquire` 可见；非 Full 的 seq
            // 是旧值，只当候选过滤，不作状态判断；失配槽永不碰，不丢未来帧）。
            if slot.state() == SlotState::Full
                && unsafe { (*ctx.seqs.add(owner * SLOTS_PER_CORE + s)).load(Ordering::Acquire) }
                    == fi
                && slot.try_acquire_drain()
            {
                return Some(s);
            }
        }
        if tsc_now() >= deadline {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// BSP 上传槽帧（CPU 直写出屏基址；OSD 行区跳过，超视频行已是槽零）。
/// 调用方已 `try_acquire_drain`，本函数上传后 `release_empty`。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
unsafe fn upload_slot(ctx: &SlotTask, owner: usize, s: usize, out_base: *mut u8) {
    unsafe {
        let copy_w = CopyWidth::from_u8(ctx.copy);
        let ring = &ctx.rings[owner];
        let slot = ring.get(s).unwrap_or_else(|| unreachable!());
        let bytes = slot.as_bytes();
        // 槽帧为 stride 对齐全帧（`scr_h * stride_bytes`），OSD 行上传跳过。
        for y in OSD_ROWS..ctx.scr_h {
            let d = out_base.add(y * ctx.stride_bytes);
            let src = bytes.as_ptr().add(y * ctx.stride_bytes);
            copy_block(d, src, ctx.stride_bytes, copy_w);
        }
        slot.release_empty();
    }
}

/// BSP 循环：按 fi 递增消费 → 上传 → 限速/独判掉帧 → OSD/里程碑（旧串不变）。
fn bsp_loop(ctx: &SlotTask) {
    let mut fi = 0usize;
    let mut skip = 0usize;
    let mut iter = 0usize;
    let mut drops_total: u64 = 0;
    // 混合 Blt 整推失败只记一次 warning（随后轮次静默走 CPU 回退）。
    let mut push_blt_warned = false;
    let mut start_ticks = rdtsc();
    let mut fps_counter = 0u32;
    let mut last_sample = start_ticks;
    let stride_px = ctx.stride_bytes / 4;
    // 120Hz 视频假设与手写路一致（`VIDEO_NATIVE_FPS`）。
    let target_ticks_per_frame = ctx.tps as u128 / 120;
    let total = ctx.source.total().max(1);
    loop {
        let cur = (fi + skip) % total;
        let fstart = rdtsc();
        // 后缓冲出屏基址：有影子时画本轮槽（按轮次取模，BSP 独定），
        // 随后整推前台；无影子直写前台（现状，零改动）。
        let sh_slot: *mut u8 = if ctx.sh_n > 0 {
            unsafe { *ctx.sh_slots.add(iter % ctx.sh_n) }
        } else {
            core::ptr::null_mut()
        };
        let out_base = if sh_slot.is_null() {
            ctx.fb_base
        } else {
            sh_slot
        };
        // 消费（超时即掉帧跳过，不断流，见模块头）。
        match bsp_acquire_frame(ctx, cur) {
            Some(s) => unsafe {
                upload_slot(
                    ctx,
                    ctx.source.owner(cur, ctx.num_cores.max(1)),
                    s,
                    out_base,
                )
            },
            None => drops_total += 1,
        }
        // OSD/统计（BSP 独画进出屏基址，与 `mp::play_task` 同串同像素语义）。
        {
            let end_ticks = rdtsc();
            let delta_ticks = (end_ticks.saturating_sub(start_ticks)) as u128;
            let ft_us = (delta_ticks * 1_000_000) / ctx.tps.max(1) as u128;
            let margin_us = if target_ticks_per_frame > delta_ticks {
                ((target_ticks_per_frame - delta_ticks) * 1_000_000) / ctx.tps.max(1) as u128
            } else {
                0
            };
            fps_counter += 1;
            if (fps_counter & 63) == 0 {
                let total_span = end_ticks.saturating_sub(last_sample);
                let fps = (64 * ctx.tps as u128) / (total_span as u128).max(1);
                last_sample = end_ticks;
                let fb = out_base as *mut u32;
                let fps_str = format!("FPS: {:>4}", fps);
                let ft_str = format!("FT: {:>5} us", ft_us);
                let mg_str = format!("Margin: {:>5} us Drop:{}", margin_us, drops_total);
                unsafe {
                    draw_string_opaque(fb, stride_px, 0, 0, fps_str.as_bytes(), 0x00FF00);
                    draw_string_opaque(fb, stride_px, 200, 0, ft_str.as_bytes(), 0x00FFFF);
                    draw_string_opaque(fb, stride_px, 450, 0, mg_str.as_bytes(), 0xFFA500);
                }
            }
            start_ticks = end_ticks;
        }
        // 影子整推（BSP 独推；Blt 失败退回 CPU 整推，只记一次 warning）。
        if ctx.sh_n > 0 {
            let px = ctx.sh_h.saturating_mul(ctx.sh_row) / 4;
            let slot = unsafe { *ctx.sh_slots.add(iter % ctx.sh_n) };
            let ok = unsafe {
                let buf = core::slice::from_raw_parts(slot as *const BltPixel, px);
                (*ctx.gop)
                    .blt(BltOp::BufferToVideo {
                        buffer: buf,
                        src: BltRegion::Full,
                        dest: (0, 0),
                        dims: (ctx.sh_row / 4, ctx.sh_h),
                    })
                    .is_ok()
            };
            if !ok {
                if !push_blt_warned {
                    push_blt_warned = true;
                    log::warn!("play: hybrid Blt push failed, CPU fallback");
                }
                unsafe {
                    copy_block(
                        ctx.fb_base,
                        slot,
                        ctx.sh_h * ctx.sh_row,
                        CopyWidth::from_u8(ctx.copy),
                    )
                };
            }
        }
        // 限速 + 独判掉帧（BSP 说了算；AP 按序产帧，不判帧）。
        if ctx.target_fps > 0 {
            let budget = ctx.tps.max(1) / ctx.target_fps.max(1) as u64;
            while rdtsc().saturating_sub(fstart) < budget {
                core::hint::spin_loop();
            }
            let late = rdtsc().saturating_sub(fstart) > budget.saturating_mul(2);
            if late {
                drops_total += 1;
            }
            skip = late as usize;
        } else {
            skip = 0;
        }
        fi = (cur + 1) % total;
        iter += 1;
        // 里程碑：BSP 每 256 轮报一次（串口-only；卡死时最后行号即定位）。
        if (iter & 255) == 0 {
            log::warn!("play: alive round={} drops={}", iter, drops_total);
        }
    }
}

/// BSP scan-split（`mp_draw_slots` 落点前半）：读片源 + 复用已有分块扫描，
/// 返回常驻只读编码源 + (视频行字节, 视频高)。空集直接 `video: no valid frames`。
/// - qois：整文件读（load 期同路径已读过一次，此处为真解码会话再读一次，
///   一次性 I/O；不复用 `Clip.lazy` 内部编码块——`source` 模块只读，字段不可见）。
/// - raw：只枚举名表（`list_raw_names` 本体），帧字节 AP 按需现读，不预读。
fn scan_split(fs: &mut Fs) -> Result<(SlotSource, usize, usize)> {
    match config::play_format() {
        MediaFormat::Qois => {
            let file = fs.open_file(cstr16!("1080p\\video.qois"))?;
            let data = Clip::read_whole_file(file, "video")?;
            let ranges = scan_qois_chunks(&data);
            if ranges.is_empty() {
                return Err(crate::error::NyaStatus::_Debug(alloc::format!(
                    "video: no valid frames"
                )));
            }
            // 几何取首个可解块（坏首块跳过、后继异形块 AP 侧判 drop，
            // 与 eager“混入异形帧跳过”同策略，计数口径见 BSP drops）。
            let mut geom = None;
            for r in &ranges {
                if let Some(p) = data.get(r.clone()) {
                    if let Ok(h) = qoi::decode_header(p) {
                        let n = (h.width * h.height) as usize;
                        if n > 0 && n <= 16 * 1024 * 1024 {
                            geom = Some((h.width as usize, h.height as usize));
                            break;
                        }
                    }
                }
            }
            let (w, h) = match geom {
                Some(g) => g,
                None => {
                    return Err(crate::error::NyaStatus::_Debug(alloc::format!(
                        "video: no valid frames"
                    )));
                }
            };
            let total = ranges.len();
            let data_leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
            let ranges_leaked: &'static [Range<usize>] = Box::leak(ranges.into_boxed_slice());
            log::info!("mp: slots scan qois chunks={} {}x{}", total, w, h);
            Ok((
                SlotSource::Qois {
                    data: data_leaked,
                    ranges: ranges_leaked,
                    w,
                    h,
                    total,
                },
                w.saturating_mul(4),
                h,
            ))
        }
        _ => {
            let names = list_raw_names(fs, "1080p", usize::MAX, "video")?;
            if names.is_empty() {
                return Err(crate::error::NyaStatus::_Debug(alloc::format!(
                    "video: no valid frames"
                )));
            }
            let total = names.len();
            let names_leaked: &'static [String] = Box::leak(names.into_boxed_slice());
            log::info!("mp: slots scan raw frames={} 1920x1080", total);
            Ok((
                SlotSource::Raw {
                    dir: "1080p",
                    names: names_leaked,
                    frame_bytes: VIDEO_W * VIDEO_H * 4,
                    total,
                },
                VIDEO_W * 4,
                VIDEO_H,
            ))
        }
    }
}

/// 帧分区槽环播放（`mp_draw` 分发落点；`clip` 为 lazy 编码源占位——真解码走
/// `scan_split` 的独立会话，`clip` 窗缓冲至此功成身退，直接 drop 省内存）。
/// `mp` 由调用方 `mp_draw` 传入（同句柄独占协议不可二次 `open_protocol_exclusive`，
/// 重开即 `ACCESS_DENIED`，故复用上游已开会话）。
pub(crate) fn mp_draw_slots(
    screen: &mut Screen,
    clip: Clip,
    target: Option<u32>,
    tps: u64,
    mp: &MpServices,
) -> Result {
    // 选中集（`crew` 决议：BSP-id 自检 + 预算/记忆/workers 优先级；槽路隐含
    // k > 1，环/分块/owner 全按 k 算，闲 AP 由 `slot_task` 入口早退）。
    let detected = mp.get_number_of_processors()?.enabled.max(1);
    let n_cores = config::resolve_crew_count(detected, mp.who_am_i().unwrap_or(0));
    drop(clip);
    let mut fs = Fs::new()?;
    let (source, row_bytes, vid_h) = scan_split(&mut fs)?;
    let (scr_w, scr_h) = screen.get_gop().current_mode_info().resolution();
    let (scr_w, scr_h) = (scr_w as usize, scr_h as usize);
    let stride_bytes = screen.get_gop().current_mode_info().stride() * 4;
    let slot_len = scr_h.saturating_mul(stride_bytes);
    if slot_len == 0 {
        Err(Status::INVALID_PARAMETER)?
    }
    let _ = scr_w;
    let fkey = match config::play_format() {
        MediaFormat::Raw => "raw",
        _ => "qois",
    };
    let feed = config::effective_feed(config::play_format());
    log::info!(
        "play: path=mp_slots frame-parallel fmt={} fetch={} cores={} slots={}",
        fkey,
        feed.as_str(),
        n_cores,
        n_cores * SLOTS_PER_CORE,
    );
    // 每核一环（`rings[c]` 只核 c 写，BSP 只读消费；backing 全零首填，
    // AP 复用时 `pack_frame` 每帧重写视频行 + 置零其余，无残留）。
    let mut backing: Vec<u8> = Vec::new();
    if backing
        .try_reserve_exact(n_cores * SLOTS_PER_CORE * slot_len)
        .is_err()
    {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "video: slots backing alloc failed"
        )));
    }
    backing.extend(core::iter::repeat(0u8).take(n_cores * SLOTS_PER_CORE * slot_len));
    let backing_leaked: &'static mut [u8] = Box::leak(backing.into_boxed_slice());
    let mut rings_owned: Vec<SlotRing<SLOTS_PER_CORE>> = Vec::new();
    for c in 0..n_cores {
        let ptr = unsafe {
            backing_leaked
                .as_mut_ptr()
                .add(c * SLOTS_PER_CORE * slot_len)
        };
        rings_owned.push(unsafe { SlotRing::new(ptr, slot_len, SLOTS_PER_CORE) });
    }
    let rings: &'static [SlotRing<SLOTS_PER_CORE>] = Box::leak(rings_owned.into_boxed_slice());
    let mut seqv: Vec<AtomicUsize> = Vec::new();
    for _ in 0..n_cores * SLOTS_PER_CORE {
        seqv.push(AtomicUsize::new(usize::MAX));
    }
    let seqs: &'static [AtomicUsize] = Box::leak(seqv.into_boxed_slice());
    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr();
    // GOP 裸指针只给 BSP 调 Blt 整推用（AP 永不碰 GOP）。
    // `mp_draw_slots` 帧内不返回（`slot_task` 死循环），Screen 活着，安全。
    let gop_ptr = &mut **screen.get_gop() as *mut GraphicsOutput;
    // 后缓冲影子环（`tune.buffers > 0` 时；分配失败退回直写，同 `mp_draw`）。
    let mut sh_n = config::tune_buffers().min(16) as usize;
    let (sh_slots_ptr, sh_row, sh_h): (*const *mut u8, usize, usize) = if sh_n == 0 {
        (core::ptr::null(), 0, 0)
    } else {
        let len = scr_h.saturating_mul(stride_bytes);
        let mut ring: Vec<Vec<u8>> = Vec::new();
        let mut ok = ring.try_reserve_exact(sh_n).is_ok() && len > 0;
        if ok {
            for _ in 0..sh_n {
                let mut s: Vec<u8> = Vec::new();
                if s.try_reserve_exact(len).is_err() {
                    ok = false;
                    break;
                }
                s.extend(core::iter::repeat(0u8).take(len));
                ring.push(s);
            }
            ok = ok && ring.len() == sh_n;
        }
        if !ok {
            log::warn!(
                "video: shadow ring alloc failed ({} bufs), fallback direct",
                sh_n
            );
            sh_n = 0;
            (core::ptr::null(), 0, 0)
        } else {
            let leaked: &'static mut [Vec<u8>] = Box::leak(ring.into_boxed_slice());
            let mut ptrs: Vec<*mut u8> = Vec::new();
            for s in leaked.iter_mut() {
                ptrs.push(s.as_mut_ptr());
            }
            (
                Box::leak(ptrs.into_boxed_slice()).as_ptr(),
                stride_bytes,
                scr_h,
            )
        }
    };
    let ctx = Box::leak(Box::new(SlotTask {
        mp: &mp,
        fb_base,
        stride_bytes,
        scr_h,
        stride_row_bytes: row_bytes,
        vid_h,
        num_cores: n_cores,
        source,
        rings,
        seqs: seqs.as_ptr(),
        copy: config::copy_width() as u8,
        target_fps: target.unwrap_or(0),
        tps,
        sh_slots: sh_slots_ptr,
        sh_n,
        sh_row,
        sh_h,
        gop: gop_ptr,
    }));
    let arg_ptr = ctx as *mut _ as *mut c_void;
    let event = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None)? };
    // 启动 AP（非阻塞；失败不静默——缺席核的帧会被 bounded 等待 drop，
    // 播放降级不断流）。
    if n_cores > 1 {
        if let Err(e) = mp.startup_all_aps(false, slot_task, arg_ptr, Some(event), None) {
            log::warn!(
                "play: startup_all_aps failed: {:?}, BSP drains bands alone",
                e.status()
            );
        }
    }
    // BSP 亲自执行（`slot_task` 按 `who_am_i` 分流，BSP 进 `bsp_loop`）。
    slot_task(arg_ptr);
    Ok(())
}
