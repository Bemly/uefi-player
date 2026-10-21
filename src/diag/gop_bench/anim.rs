//! `gop_bench` 动画帧率矩阵子模块（`AnimCtx` + `anim_cell` + `wait_done`）。
//! 由 `super::run_section` 调用（核数 × 限帧）；拆分仅为满足单文件 ≤1000 行，不改行为。

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use uefi::boot::{EventType, Tpl, create_event, stall};
use uefi::proto::pi::mp::MpServices;

use crate::error::{NyaStatus, Result};
use crate::graphics::Screen;

use super::fill::rows_for;

macro_rules! tline {
    ($report:expr, $screen:expr, $($arg:tt)*) => {{
        let s = alloc::format!($($arg)*);
        log::info!("{}", s);
        $screen.draw_str(&s);
        $report.push(s);
    }};
}

#[inline(always)]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// 预生成帧数（内存占用 4×8MB@1080p）。
pub(crate) const ANIM_FRAMES: usize = 4;
/// 每格计时帧数（限帧格 ≈2s@60fps）。
pub(crate) const ANIM_TIMED: u64 = 120;
/// 退出哨兵。
const ANIM_QUIT: usize = usize::MAX;

/// 一格的 AP 启动方式。
#[derive(Clone, Copy)]
pub(crate) enum AnimWorkers {
    /// 仅 BSP，无 AP。
    BspOnly,
    /// 定点启动 1 个 AP（ bloß 参数为其处理器号）。
    OneAp(usize),
    /// 启动全部 AP。
    All,
}

struct AnimCtx<'a> {
    mp: &'a MpServices,
    fb_base: *mut u8,
    fb_stride: usize,
    frames: *const u8,
    frame_bytes: usize,
    row_bytes: usize,
    height: usize,
    /// halves=true 时 AP 取下半区（双核格）；false 时按 id 分区（全核格）。
    halves: bool,
    n: usize,
    ap_y0: usize,
    ap_y1: usize,
    round: &'a AtomicUsize,
    done: &'a AtomicUsize,
}

/// AP 动画循环：按轮次号拷对应帧的自己行区。帧号由轮次推导，
/// 与 BSP 侧保持同一公式，保证同轮同帧（撕裂只发生在帧边界，不混帧）。
extern "efiapi" fn ap_anim_loop(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const AnimCtx) };
    let (y0, y1) = if ctx.halves {
        (ctx.ap_y0, ctx.ap_y1)
    } else {
        let id = ctx.mp.who_am_i().unwrap_or(usize::MAX) % ctx.n.max(1);
        rows_for(id, ctx.n, ctx.height)
    };
    let mut my_round = 0usize;
    loop {
        let g = ctx.round.load(Ordering::Acquire);
        if g == ANIM_QUIT {
            break;
        }
        if g == 0 || g == my_round {
            core::hint::spin_loop();
            continue;
        }
        my_round = g;
        let fptr = unsafe { ctx.frames.add(((g - 1) % ANIM_FRAMES) * ctx.frame_bytes) };
        unsafe { copy_rows(ctx.fb_base, ctx.fb_stride, fptr, ctx.row_bytes, y0, y1) };
        ctx.done.fetch_add(1, Ordering::SeqCst);
    }
}

/// 从显存行区视角拷一帧的行区（帧行宽与显存 stride 可能不同，逐行拷）。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
pub(crate) unsafe fn copy_rows(
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

/// 等本轮 AP 打卡齐，单轮 5 秒熔断。返回是否齐（false=熔断）。
fn wait_done(done: &AtomicUsize, target: usize, tps: u64) -> bool {
    let deadline = rdtsc() + 5 * tps.max(1);
    while done.load(Ordering::Acquire) < target {
        if rdtsc() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// 跑一格动画矩阵：热身 1 轮（吸收 AP 唤醒）+ 计时 N 轮。
/// paced=true 时每帧按 60fps 预算自旋（软件限帧），超预算记 overrun。
/// 返回 free-run 等价 fps（paced 格约等于 60，看 overruns）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn anim_cell(
    report: &mut Vec<String>,
    screen: &mut Screen,
    tps: u64,
    mp: Option<&MpServices>,
    fb_base: *mut u8,
    fb_stride: usize,
    frames: *const u8,
    frame_bytes: usize,
    row_bytes: usize,
    h: usize,
    bsp_y0: usize,
    bsp_y1: usize,
    workers: AnimWorkers,
    label: &str,
    paced: bool,
) -> Result<f64> {
    let round = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    // 期望的每轮 AP 打卡数（BSP 不打卡，只管自己那份）。
    let expected = match workers {
        AnimWorkers::BspOnly => 0,
        AnimWorkers::OneAp(_) => 1,
        AnimWorkers::All => {
            let n = mp
                .and_then(|m| m.get_number_of_processors().ok())
                .map(|c| c.enabled)
                .unwrap_or(1);
            n.saturating_sub(1)
        }
    };

    // 启动 AP（BSP-only 格跳过）。AP 进常驻循环等轮次号。
    // event 绑定到函数尾：非阻塞启动依赖它存活（同 video.rs 做法）。
    if expected > 0 {
        let m = mp.ok_or(NyaStatus::_Debug(String::from("no MP")))?;
        let (halves, n, ap_y0, ap_y1) = match workers {
            AnimWorkers::OneAp(_) => (true, 2, h / 2, h),
            _ => (
                false,
                m.get_number_of_processors().map(|c| c.enabled).unwrap_or(1),
                0,
                0,
            ),
        };
        let ctx = AnimCtx {
            mp: m,
            fb_base,
            fb_stride,
            frames,
            frame_bytes,
            row_bytes,
            height: h,
            halves,
            n,
            ap_y0,
            ap_y1,
            round: &round,
            done: &done,
        };
        let arg_ptr = &ctx as *const _ as *mut c_void;
        let event = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None)? };
        // Event 无 Drop，移进启动调用即可（固件侧保持有效，同 video.rs）。
        match workers {
            AnimWorkers::OneAp(idx) => {
                m.startup_this_ap(idx, ap_anim_loop, arg_ptr, Some(event), None)
                    .map_err(|e| e.status())?;
            }
            _ => {
                m.startup_all_aps(false, ap_anim_loop, arg_ptr, Some(event), None)
                    .map_err(|e| e.status())?;
            }
        }
    }

    let budget = tps / 60;
    // 热身轮（frame 0），不计时。
    round.store(1, Ordering::SeqCst);
    unsafe { copy_rows(fb_base, fb_stride, frames, row_bytes, bsp_y0, bsp_y1) };
    let mut degraded = !wait_done(&done, expected, tps);
    let mut overruns = 0u64;
    let t0 = rdtsc();
    for r in 2..2 + ANIM_TIMED as usize {
        let fstart = rdtsc();
        round.store(r, Ordering::SeqCst);
        let fptr = unsafe { frames.add(((r - 1) % ANIM_FRAMES) * frame_bytes) };
        unsafe { copy_rows(fb_base, fb_stride, fptr, row_bytes, bsp_y0, bsp_y1) };
        if !wait_done(&done, expected * r, tps) {
            degraded = true;
            break;
        }
        let fend = rdtsc();
        if paced {
            if fend - fstart > budget {
                overruns += 1;
            }
            while rdtsc() < fstart + budget {
                core::hint::spin_loop();
            }
        }
    }
    let dt = rdtsc() - t0;
    if expected > 0 {
        round.store(ANIM_QUIT, Ordering::SeqCst);
        // 给 AP 一个循环周期退出 procedure，之后 ctx/event 才随栈释放。
        // 不等的话 AP 可能读到已释放的栈 → #GP。
        stall(Duration::from_millis(50));
    }
    let fps = ANIM_TIMED as f64 * tps as f64 / dt.max(1) as f64;
    let ms = dt as f64 * 1000.0 / (ANIM_TIMED as f64 * tps as f64);
    if paced {
        tline!(
            report,
            screen,
            "anim {} vsync60: {} frames, {:.1} fps, overruns {}{}",
            label,
            ANIM_TIMED,
            fps,
            overruns,
            if degraded { " TIMEOUT?" } else { "" }
        );
    } else {
        tline!(
            report,
            screen,
            "anim {} free: {} frames, {:.1} fps ({:.2} ms/f){}",
            label,
            ANIM_TIMED,
            fps,
            ms,
            if degraded { " TIMEOUT?" } else { "" }
        );
    }
    Ok(fps)
}
