//! `graphics::mp_copy`：TSC/校准计时 + 多核校准会话 + 位宽拷贝族。
//! `tsc`/`calibrate_tps` 供 `hevc_probe`/`menu`/`idle`/`boot` 计时；
//! `dynamic_row_bounds`/`copy_block` 供 `video::mp_draw` 与上屏路径按位宽分发。
//! 拆分仅为满足单文件 ≤1000 行，不改行为。
//!
//! 统一控制面搬运核：BSP/AP 行区分工（`copy_block` 位宽选路），AP 永不碰 GOP/Blt；
//! 解码-while-上传流水的上传半即此（BSP 先解后推，AP 拷影子/前台行区，BSP 整推）。
//!
//! SeqGate 行边界发布（播放侧 `video::mp` dynamic 路用；校准侧 `MpCalCtx.bounds`
//! + `mode`/`round` 原封不动——校准数组分配在只读 `calibrate.rs`，行为零改，
//! 只换播放侧的发布/读取机制）：
//! - 新标记行（矩阵 follow-up 断言用，不断言画质）：
//!   `mp: bound SeqGate armed n=<cores>`（`mp_draw` 建门时记一笔，
//!   每会话一次；发布/读取本身静默，重试上限 5·tps，见 `read_bound_gate`）。
//! - `BoundGate` 定长 129（≤128 核；上限与 `exec`  steal 队深同级，
//!   超限 dynamic 在 `mp_draw` 侧回落 static，见该处注释）。

use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use core::time::Duration;

use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::pi::mp::MpServices;

use crate::config::{self, CopyWidth};
use crate::video::sync::SeqGate;

/// 时间戳计数。非 x86_64 返回 0（auto 会退回 direct）。
#[inline(always)]
pub(crate) fn tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// TSC 频率校准（100ms × 2，和 `hw_bench` 同方法）。vsync 预算/ half 取半用。
pub(crate) fn calibrate_tps() -> u64 {
    uefi::boot::stall(Duration::from_millis(100));
    let s = tsc();
    uefi::boot::stall(Duration::from_millis(100));
    tsc().saturating_sub(s).saturating_mul(10).max(1)
}

// ---------- dynamic 行边界的 SeqGate 发布（播放侧；校准侧沿用 raw 数组） ----------

/// SeqGate 上限核数（`BoundGate` 定长 129 = 128 核 + 首项；`exec` steal 队深 64 同级，
/// 播放 dynamic 实测覆盖不到的量级，超限由 `mp_draw` 回落 static，见该处）。
pub(crate) const BOUND_GATE_MAX_CORES: usize = 128;

/// dynamic 行边界发布门（n+1 项行号，定长 129 槽；BSP 独写、AP 无锁读，
/// 协议见 `video::sync::SeqGate`）。校准会话不用它（分配点在只读 `calibrate.rs`）。
pub(crate) type BoundGate = SeqGate<129>;

/// 发布整组行边界（BSP 独写；`bounds.len()` 须为 n+1 且 n ≤ 128，
/// 超长截断——调用方已按 `BOUND_GATE_MAX_CORES` 回落，此处只兜底）。
pub(crate) fn publish_bound_gate(gate: &BoundGate, bounds: &[usize]) {
    let mut arr = [0usize; 129];
    let n = bounds.len().min(129);
    arr[..n].copy_from_slice(&bounds[..n]);
    gate.publish(&arr);
}

/// 快照读（AP 侧；`n` 为有效项数 = 核数+1，`out.len()` 须 ≥ n）。
/// tsc-bounded 重试（5·`tps` 熔断，同 `sync::wait_state_eq` 口径）：
/// 到界返 false（调用方沿用旧行区——行区只影响负载分布，不影响正确性）。
pub(crate) fn read_bound_gate(gate: &BoundGate, n: usize, out: &mut [usize], tps: u64) -> bool {
    let deadline = tsc().saturating_add(5 * tps.max(1));
    let mut tmp = [0usize; 129];
    loop {
        if gate.try_read(&mut tmp) {
            let m = n.min(129).min(out.len());
            out[..m].copy_from_slice(&tmp[..m]);
            return true;
        }
        if tsc() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

// ---------- 矩阵校准的多核会话 ----------

/// AP 退场哨兵：BSP 写 QUIT 后 AP 跳出常驻循环（同 `gop_bench::FILL_QUIT` 做法）。
pub(crate) const CAL_QUIT: usize = usize::MAX;

/// 模式：0=static, 1=tile, 2=dynamic（三 cell 共用一个会话，轮次间翻转）。
pub(crate) const CAL_MODE_STATIC: u8 = 0;
pub(crate) const CAL_MODE_TILED: u8 = 1;
pub(crate) const CAL_MODE_DYNAMIC: u8 = 2;

pub(crate) struct MpCalCtx<'a> {
    pub(crate) mp: &'a MpServices,
    pub(crate) fb_base: *mut u8,
    pub(crate) fb_stride: usize,
    pub(crate) src: *const u8,
    pub(crate) row_bytes: usize,
    pub(crate) height: usize,
    pub(crate) n: usize,
    pub(crate) round: &'a AtomicUsize,
    pub(crate) done: &'a AtomicUsize,
    pub(crate) mode: &'a AtomicU8,
    pub(crate) tile: &'a AtomicUsize,
    pub(crate) tile_rows: usize,
    pub(crate) tiles_total: usize,
    /// 各核上一轮静态 copy 耗时（AP 自记；BSP 读来算 dynamic 加权边界）。
    pub(crate) times: *const AtomicUsize,
    /// dynamic 加权行边界（n+1 项；BSP 写、mode/round 发布后 AP 只读）。
    pub(crate) bounds: *const AtomicUsize,
    /// 选中集上限（`config::crew` 决议的 k；`cap == n` 为旧行为全员参加）。
    /// crew 外 AP（`who_am_i >= cap`）idle 自旋到 QUIT，不拷行区、不打卡；
    /// BSP 侧 `MpDrive.expected` 须按 `cap` 配（`cap - 1`），否则打卡数对不上。
    pub(crate) cap: usize,
}

/// AP 常驻循环：等新轮次号，按本轮模式拷（静态分区 / tile 游标领活 /
/// dynamic 加权边界），打卡。静态分支自带计时（dynamic 定权 probe 用）。
/// BSP 发 QUIT 后退出。mode/bounds 由 BSP 在发布轮次号前写好
///（SeqCst，先 mode/bounds 后 round）。
pub(crate) extern "efiapi" fn ap_cal_copy(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const MpCalCtx) };
    // 选中集门：crew 外 AP idle 到 QUIT（不拷行区、不打卡；BSP-id 假设与
    // start-all-but-idle-unchosen 理由见 `config::crew` 模块头）。
    let phys = ctx.mp.who_am_i().unwrap_or(usize::MAX);
    if phys >= ctx.cap.max(1) {
        loop {
            if ctx.round.load(Ordering::Acquire) == CAL_QUIT {
                break;
            }
            core::hint::spin_loop();
        }
        return;
    }
    let id = phys % ctx.n.max(1);
    let (y0, y1) = rows_for_cal(id, ctx.n, ctx.height);
    let mut my_round = 0usize;
    loop {
        let g = ctx.round.load(Ordering::Acquire);
        if g == CAL_QUIT {
            break;
        }
        if g == 0 || g == my_round {
            core::hint::spin_loop();
            continue;
        }
        my_round = g;
        let mode = ctx.mode.load(Ordering::Acquire);
        if mode == CAL_MODE_TILED {
            let base = (g as u64).saturating_mul(ctx.tiles_total as u64);
            loop {
                let t = ctx.tile.fetch_add(1, Ordering::SeqCst) as u64;
                let rel = t.saturating_sub(base) as usize;
                if rel >= ctx.tiles_total {
                    break;
                }
                let ty0 = rel * ctx.tile_rows;
                let ty1 = (ty0 + ctx.tile_rows).min(ctx.height);
                unsafe {
                    copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, ty0, ty1)
                };
            }
        } else if mode == CAL_MODE_DYNAMIC {
            let dy0 = unsafe {
                (*ctx.bounds.add(id))
                    .load(Ordering::Acquire)
                    .min(ctx.height)
            };
            let dy1 = unsafe {
                (*ctx.bounds.add(id + 1))
                    .load(Ordering::Acquire)
                    .min(ctx.height)
                    .max(dy0)
            };
            unsafe { copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, dy0, dy1) };
        } else {
            let t0 = tsc();
            unsafe { copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, y0, y1) };
            unsafe {
                (*ctx.times.add(id)).store(tsc().saturating_sub(t0) as usize, Ordering::Relaxed)
            };
        }
        ctx.done.fetch_add(1, Ordering::SeqCst);
    }
}

/// BSP 侧驱动器：发轮次号 → 拷自己份（分区/领活/加权边界，影子时代画影子）→
/// 等 AP 打卡齐（单轮 5 秒熔断）→ 混合整推（见 `maybe_push`）。
pub(crate) struct MpDrive<'a> {
    pub(crate) n: usize,
    pub(crate) bsp_id: usize,
    pub(crate) fb_base: *mut u8,
    pub(crate) fb_stride: usize,
    pub(crate) src: *const u8,
    pub(crate) row_bytes: usize,
    pub(crate) height: usize,
    pub(crate) bsp_y0: usize,
    pub(crate) bsp_y1: usize,
    pub(crate) expected: usize,
    pub(crate) round: &'a AtomicUsize,
    pub(crate) done: &'a AtomicUsize,
    pub(crate) mode: &'a AtomicU8,
    pub(crate) tile: &'a AtomicUsize,
    pub(crate) tile_rows: usize,
    pub(crate) tiles_total: usize,
    pub(crate) times: *const AtomicUsize,
    pub(crate) bounds: *const AtomicUsize,
    /// 混合整推（`buffers>0` 时启用）：`fb_base` 建会话时已指向影子槽
    /// （AP/BSP 画影子）；`real_fb` 为真前台；`gop` 裸指针只给 BSP
    /// 调 Blt 用（AP 永不碰 GOP：独占协议 + 固件 Blt 非线程安全）。
    /// 直写时 `fb_base` 即真前台，行为与旧版一致。
    pub(crate) real_fb: *mut u8,
    pub(crate) gop: *mut GraphicsOutput,
    pub(crate) blt_push: bool,
    pub(crate) push_warned: bool,
    pub(crate) next: usize,
    pub(crate) tps: u64,
}

impl MpDrive<'_> {
    /// 热身 1 轮静态分区（吸收 AP 唤醒延迟，不计时）。
    pub(crate) fn warmup_static(&mut self) {
        let _ = self.run_round();
    }

    /// 热身 1 轮 tile（同上）。
    pub(crate) fn warmup_tiled(&mut self) {
        let _ = self.run_round_tiled();
    }

    /// 热身 1 轮 dynamic（定权 probe 之后，不计时）。
    pub(crate) fn warmup_dynamic(&mut self) {
        let _ = self.run_round_dynamic();
    }

    /// dynamic 定权 probe：跑一轮静态（AP 在静态分支自带计时），读各核耗时
    /// 算加权边界并发布。返回 false = 本轮 AP 没跟上（调用方作废整个 MP 会话）。
    pub(crate) fn probe_dynamic(&mut self) -> bool {
        if !self.run_round() {
            return false;
        }
        let mut times = vec![0u64; self.n.max(1)];
        for (i, t) in times.iter_mut().enumerate() {
            *t = unsafe { (*self.times.add(i)).load(Ordering::Relaxed) } as u64;
        }
        let b = dynamic_row_bounds(&times, self.n, 0, self.height);
        for (i, v) in b.iter().enumerate() {
            unsafe { (*self.bounds.add(i)).store(*v, Ordering::Relaxed) };
        }
        true
    }

    /// 跑一轮 dynamic 加权分区（BSP 拷自己的加权行区；AP 按发布的边界拷）。
    pub(crate) fn run_round_dynamic(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        self.mode.store(CAL_MODE_DYNAMIC, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        let dy0 = unsafe { (*self.bounds.add(self.bsp_id)).load(Ordering::Relaxed) };
        let dy1 = unsafe { (*self.bounds.add(self.bsp_id + 1)).load(Ordering::Relaxed) };
        let dy0 = dy0.min(self.height);
        let dy1 = dy1.min(self.height).max(dy0);
        unsafe {
            copy_rows_cal(
                self.fb_base,
                self.fb_stride,
                self.src,
                self.row_bytes,
                dy0,
                dy1,
            )
        };
        if !self.wait_done(r) {
            return false;
        }
        self.maybe_push()
    }

    /// 跑一轮静态分区，返回是否全员跟上（false = 熔断，调用方作废 MP cell）。
    /// 混合模式下拷的是影子（`fb_base` 建会话时已指向影子），随后 BSP 整推。
    pub(crate) fn run_round(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        self.mode.store(CAL_MODE_STATIC, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        unsafe {
            copy_rows_cal(
                self.fb_base,
                self.fb_stride,
                self.src,
                self.row_bytes,
                self.bsp_y0,
                self.bsp_y1,
            )
        };
        if !self.wait_done(r) {
            return false;
        }
        self.maybe_push()
    }

    /// 跑一轮 tile 自调度（游标按轮次编址 `[r*N, (r+1)*N)`，BSP 也领活）。
    pub(crate) fn run_round_tiled(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        let base = (r as u64).saturating_mul(self.tiles_total as u64);
        self.tile.store(base as usize, Ordering::SeqCst);
        self.mode.store(CAL_MODE_TILED, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        loop {
            let t = self.tile.fetch_add(1, Ordering::SeqCst) as u64;
            let rel = t.saturating_sub(base) as usize;
            if rel >= self.tiles_total {
                break;
            }
            let y0 = rel * self.tile_rows;
            let y1 = (y0 + self.tile_rows).min(self.height);
            unsafe {
                copy_rows_cal(
                    self.fb_base,
                    self.fb_stride,
                    self.src,
                    self.row_bytes,
                    y0,
                    y1,
                )
            };
        }
        if !self.wait_done(r) {
            return false;
        }
        self.maybe_push()
    }

    /// 混合整推（BSP 独推）：Blt 引擎 `BufferToVideo` 影子→前台（一槽整屏）。
    /// AP 不碰 GOP。引擎失败退回 CPU 整推（同字节，只多一次搬运），计时照收，
    /// 只记一次 warning（bench 照跑，不断矩阵）。
    pub(crate) fn maybe_push(&mut self) -> bool {
        if !self.blt_push {
            return true;
        }
        // 单位口径：`fb_stride`/`row_bytes` 是字节，Blt 吃像素。影子槽是
        // `height × stride_px` 像素；推屏宽取可见宽（`row_bytes/4`），不能拿步幅当宽——
        // stride ≥ 屏宽，`dims` 超屏宽即 uefi `check_framebuffer_region` panic
        //（`buffers` 默认 2 起混合路首次走到，曾以字节步幅 7680 当宽炸矩阵）。
        let stride_px = self.fb_stride / 4;
        let w_px = (self.row_bytes / 4).min(stride_px);
        let px = self.height.saturating_mul(stride_px);
        let ok = unsafe {
            let buf = core::slice::from_raw_parts(self.fb_base as *const BltPixel, px);
            (*self.gop)
                .blt(BltOp::BufferToVideo {
                    buffer: buf,
                    src: BltRegion::SubRectangle {
                        coords: (0, 0),
                        px_stride: stride_px,
                    },
                    dest: (0, 0),
                    dims: (w_px, self.height),
                })
                .is_ok()
        };
        if ok {
            return true;
        }
        if !self.push_warned {
            self.push_warned = true;
            log::warn!("render auto: hybrid Blt push failed, CPU fallback");
        }
        unsafe {
            copy_block(
                self.real_fb,
                self.fb_base,
                // `fb_stride` 已是字节，不再 ×4（同上单位口径）。
                self.height.saturating_mul(self.fb_stride),
                config::copy_width(),
            )
        };
        true
    }

    /// 等本轮 AP 打卡齐（5 秒熔断）。
    pub(crate) fn wait_done(&self, r: usize) -> bool {
        let target = r.saturating_mul(self.expected);
        let deadline = tsc().saturating_add(5 * self.tps.max(1));
        while self.done.load(Ordering::Acquire) < target {
            if tsc() >= deadline {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }
}

/// 按核 id 划分行区，尾核吃掉余数（同 `gop_bench::rows_for`）。
pub(crate) fn rows_for_cal(id: usize, n: usize, h: usize) -> (usize, usize) {
    let per = h / n.max(1);
    let y0 = id * per;
    let y1 = if id + 1 >= n { h } else { y0 + per };
    (y0.min(h), y1.min(h))
}

/// 按各核搬运耗时加权分行（dynamic 用；校准定权与播放自适应共用同一数学）。
/// `times[i]` 为核 i 上一轮 copy ticks（0 = 无数据）；`y0..y1` 为待分行区
/// （播放侧传 `OSD_ROWS..H`，校准侧传全高）。快核多拿：
/// 行数正比于速率（`SCALE / t`），最大余数法 + 每核至少 1 行（行数够分时）。
/// 全无数据 → 等分；核比行多（退化，实机到不了）→ 前面的核各 1 行。
/// 返回 n+1 项边界（首项 y0、末项 y1）。整数运算，无浮点（软浮点历史，见 AGENTS）。
pub(crate) fn dynamic_row_bounds(times: &[u64], n: usize, y0: usize, y1: usize) -> Vec<usize> {
    let n = n.max(1);
    let total = y1.saturating_sub(y0);
    let mut bounds = Vec::with_capacity(n + 1);
    bounds.push(y0);
    if total == 0 {
        for _ in 0..n {
            bounds.push(y0);
        }
        return bounds;
    }
    // 速率：耗时越小越快。0 耗时视为无数据（首轮/异常），先记 0。
    const SCALE: u64 = 1 << 24;
    let mut rates = vec![0u64; n];
    for (i, r) in rates.iter_mut().enumerate() {
        let t = times.get(i).copied().unwrap_or(0);
        if t > 0 {
            *r = SCALE / t.max(1);
        }
    }
    if rates.iter().all(|&r| r == 0) {
        // 全无数据 → 等分（首轮就这样，与 static 同分布）。
        for i in 1..=n {
            bounds.push(y0 + total * i / n);
        }
        return bounds;
    }
    // 部分无数据的核按平均速率兜底（防 0 行震荡：0 行 → 耗时 0 → 下轮爆权重）。
    let known: Vec<u64> = rates.iter().copied().filter(|&r| r > 0).collect();
    let avg = known.iter().sum::<u64>() / known.len().max(1) as u64;
    for r in rates.iter_mut() {
        if *r == 0 {
            *r = avg.max(1);
        }
    }
    let sum: u64 = rates.iter().sum();
    let mut rows = vec![0usize; n];
    if total >= n {
        // 先每核预留 1 行，剩余按权重最大余数分。
        let rest = (total - n) as u64;
        let mut fracs = vec![0u64; n];
        let mut assigned = 0usize;
        for (i, r) in rates.iter().enumerate() {
            let base = rest * r / sum.max(1);
            fracs[i] = rest * r % sum.max(1);
            rows[i] = 1 + base as usize;
            assigned += base as usize;
        }
        let mut leftover = rest as usize - assigned;
        while leftover > 0 {
            let mut pick = 0;
            for i in 1..n {
                if fracs[i] > fracs[pick] {
                    pick = i;
                }
            }
            if fracs[pick] == 0 && leftover < n {
                break;
            }
            rows[pick] += 1;
            fracs[pick] = 0;
            leftover -= 1;
        }
    } else {
        for (i, r) in rows.iter_mut().enumerate() {
            *r = if i < total { 1 } else { 0 };
        }
    }
    let mut y = y0;
    for r in rows {
        y += r;
        bounds.push(y.min(y1));
    }
    // 末项钳到 y1（整除余数场景，rows 总和恒等于 total，钳制只是兜底）。
    if let Some(last) = bounds.last_mut() {
        *last = y1;
    }
    bounds
}

/// 从合成帧行区拷到显存行区（显存 stride 可能大于行宽，逐行拷）。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
pub(crate) unsafe fn copy_rows_cal(
    dst_base: *mut u8,
    dst_stride: usize,
    src_frame: *const u8,
    row_bytes: usize,
    y0: usize,
    y1: usize,
) {
    unsafe {
        for y in y0..y1 {
            let d = dst_base.add(y * dst_stride);
            let s = src_frame.add(y * row_bytes);
            core::ptr::copy_nonoverlapping(s, d, row_bytes);
        }
    }
}

/// P2 位宽分发：安全外壳 + 运行时门控（调用方保证 CPU 支持该位宽，
/// 见 `config::copy_width`），内部分 unsafe 执行。AVX 系自带 `vzeroupper`。
/// `video::mp_draw` 的 AP/BSP 行区搬运复用同一个（同 semantics）。
pub(crate) fn copy_block(dst: *mut u8, src: *const u8, len: usize, w: CopyWidth) {
    unsafe {
        match w {
            CopyWidth::U64 => copy_block_u64(dst, src, len),
            CopyWidth::X128 => copy_block_x128(dst, src, len),
            CopyWidth::V128 => copy_block_v128(dst, src, len),
            CopyWidth::Y256 => copy_block_y256(dst, src, len),
            CopyWidth::Z512 => copy_block_z512(dst, src, len),
        }
    }
}

/// 标量兜底（`accel.level = scalar` 或史前 CPU）：u64 步进。
unsafe fn copy_block_u64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            (dst.add(i) as *mut u64).write((src.add(i) as *const u64).read_unaligned());
            i += 8;
        }
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 128 位整数向量（SSE2~SSE4.2 同宽）：`movdqu` 非对齐载存。
/// 约束见 `gop_simd.rs` 头注释：只碰整数 `asm!`，内在函数会被标量化。
#[target_feature(enable = "sse2")]
unsafe fn copy_block_x128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 128 位 VEX 编码（`accel.level = avx`）：同宽换 VEX，对照语义。
#[target_feature(enable = "avx")]
unsafe fn copy_block_v128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 256 位向量（`accel.level = avx2`）：调用前必须过 CPUID+XCR0 门控。
#[target_feature(enable = "avx2")]
unsafe fn copy_block_y256(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 512 位向量（`accel.level = avx512`）：调用前必须过 AVX512F+XCR0 五位门控。
#[target_feature(enable = "avx512f")]
unsafe fn copy_block_z512(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovdqu8 [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 标量 u64 步进底层拷贝（`legacy_u64` 管线的搬运核，无 self 依赖）。
/// 现行 `copy_block_u64`（P2 位宽分发）是它的矩阵形态；这里保留循环形态供老管线用。
#[inline(always)]
pub(crate) unsafe fn u64_fast_copy(src: *const u8, dst: *mut u8, len: usize) {
    let mut i = 0;
    while i + 64 <= len {
        let s0 = (src.add(i) as *const u64).read_unaligned();
        let s1 = (src.add(i + 8) as *const u64).read_unaligned();
        let s2 = (src.add(i + 16) as *const u64).read_unaligned();
        let s3 = (src.add(i + 24) as *const u64).read_unaligned();
        let s4 = (src.add(i + 32) as *const u64).read_unaligned();
        let s5 = (src.add(i + 40) as *const u64).read_unaligned();
        let s6 = (src.add(i + 48) as *const u64).read_unaligned();
        let s7 = (src.add(i + 56) as *const u64).read_unaligned();

        (dst.add(i) as *mut u64).write_volatile(s0);
        (dst.add(i + 8) as *mut u64).write_volatile(s1);
        (dst.add(i + 16) as *mut u64).write_volatile(s2);
        (dst.add(i + 24) as *mut u64).write_volatile(s3);
        (dst.add(i + 32) as *mut u64).write_volatile(s4);
        (dst.add(i + 40) as *mut u64).write_volatile(s5);
        (dst.add(i + 48) as *mut u64).write_volatile(s6);
        (dst.add(i + 56) as *mut u64).write_volatile(s7);
        i += 64;
    }
    while i + 8 <= len {
        (dst.add(i) as *mut u64).write_volatile((src.add(i) as *const u64).read_unaligned());
        i += 8;
    }
    while i < len {
        dst.add(i).write_volatile(src.add(i).read());
        i += 1;
    }
}
