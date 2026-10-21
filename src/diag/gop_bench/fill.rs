//! `gop_bench` 多核填充子模块（`rows_for` + `FillCtx` + AP 常驻循环）。
//! 由 `super::run_section` 调用（单核 vs 多核显存填充扩展比）；拆分仅为满足单文件 ≤1000 行，不改行为。

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use uefi::proto::pi::mp::MpServices;

/// 按核 id 划分行区，尾核吃掉余数。
pub(crate) fn rows_for(id: usize, n: usize, h: usize) -> (usize, usize) {
    let per = h / n.max(1);
    let y0 = id * per;
    let y1 = if id + 1 >= n { h } else { y0 + per };
    (y0.min(h), y1.min(h))
}

/// u64 易失填充行区（显存是 WC 内存，必须 volatile，不许编译器合并/消掉）。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
pub(crate) unsafe fn fill_rows(
    base: *mut u8,
    stride_bytes: usize,
    width_px: usize,
    y0: usize,
    y1: usize,
    pat: u64,
) {
    unsafe {
        let row_bytes = width_px * 4;
        for y in y0..y1 {
            let row = base.add(y * stride_bytes);
            let mut i = 0;
            while i + 8 <= row_bytes {
                (row.add(i) as *mut u64).write_volatile(pat);
                i += 8;
            }
        }
    }
}

pub(crate) struct FillCtx<'a> {
    pub(crate) mp: &'a MpServices,
    pub(crate) fb_base: *mut u8,
    pub(crate) stride_bytes: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) n: usize,
    pub(crate) round: &'a AtomicUsize,
    pub(crate) done: &'a AtomicUsize,
}

/// 退出哨兵：BSP 写 round=QUIT 后 AP 跳出常驻循环，procedure 返回。
pub(crate) const FILL_QUIT: usize = usize::MAX;

/// AP 常驻工作循环：等新一轮轮次号，填自己行区，打卡。BSP 发 QUIT 后退出。
pub(crate) extern "efiapi" fn ap_fill_loop(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const FillCtx) };
    let id = ctx.mp.who_am_i().unwrap_or(usize::MAX) % ctx.n.max(1);
    let (y0, y1) = rows_for(id, ctx.n, ctx.height);
    let mut my_round = 0usize;
    loop {
        let g = ctx.round.load(Ordering::Acquire);
        if g == FILL_QUIT {
            break;
        }
        if g == my_round {
            core::hint::spin_loop();
            continue;
        }
        my_round = g;
        unsafe {
            fill_rows(
                ctx.fb_base,
                ctx.stride_bytes,
                ctx.width,
                y0,
                y1,
                0x0044_5566_0044_5566,
            )
        };
        ctx.done.fetch_add(1, Ordering::SeqCst);
    }
}
