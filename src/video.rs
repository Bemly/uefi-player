use crate::error::Result;
use crate::fs::Fs;
use crate::graphics::Screen;
use crate::video::decoder::Clip;
use core::time::Duration;
use uefi::boot;
use uefi::boot::set_watchdog_timer;

pub mod ascii_font;
pub mod buffer;
pub mod decoder;
pub mod demux;
pub mod exec;
pub mod loops;
pub mod mp;
pub mod play;

pub(crate) use self::loops::{blt_loop, direct_loop};
pub use self::mp::OSD_ROWS;
pub(crate) use self::mp::mp_draw;
pub(crate) use self::play::{load_video_clip, need_eager_clip, resolve_play_target};

pub(crate) const VIDEO_W: usize = 1920;
pub(crate) const VIDEO_H: usize = 1080;
/// `.qois` 无帧率元数据，沿用 `mp_draw` 的生产假设：120Hz 视频。
/// `auto` 的上限就按它封顶（比它快也没意义）。
pub(crate) const VIDEO_NATIVE_FPS: u32 = 120;

#[inline(always)]
pub(crate) fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC 频率校准（100ms × 2，和 `hw_bench` 同方法，QEMU/真机都可用）。
pub(crate) fn calibrate_tps() -> u64 {
    boot::stall(Duration::from_millis(100));
    let s = rdtsc();
    boot::stall(Duration::from_millis(100));
    rdtsc().saturating_sub(s).saturating_mul(10).max(1)
}

/// 限速 + 掉帧（P3，单线程路）：帧内耗时超预算先自旋补齐；迟到超出一整帧则
/// 丢帧追赶（最多连丢 2 帧防雪崩）。记数只走串口（屏显不刷，不污染画面）。
pub(crate) fn pace_or_drop(
    video: &mut Clip,
    target: Option<u32>,
    tps: u64,
    fstart: u64,
    drops: &mut u64,
) {
    let Some(fps) = target else { return };
    let budget = tps.max(1) / fps.max(1) as u64;
    let dt = rdtsc().saturating_sub(fstart);
    if dt <= budget {
        while rdtsc().saturating_sub(fstart) < budget {
            core::hint::spin_loop();
        }
        return;
    }
    let mut late = dt - budget;
    let mut n = 0;
    while late >= budget && n < 2 {
        match video.next_frame() {
            Some(_) => *drops += 1,
            None => {
                video.rewind();
                break;
            }
        }
        late = late.saturating_sub(budget);
        n += 1;
    }
    if *drops > 0 && *drops % 512 == 0 {
        log::warn!("play: dropped {} frames total (late)", *drops);
    }
}

pub fn video_run(screen: &mut Screen) -> Result {
    // 关闭看门狗，如果不行之后写定时喂狗
    set_watchdog_timer(0, 0, None)?;

    let tps = calibrate_tps();
    let target = resolve_play_target(screen, tps)?;

    // 统一控制面：无旁路分发，恒经 render 矩阵 + schedule 后端。
    // 旧 `[play] pipeline` 7 档已删（未知键忽略）：staged/zero_copy/stream/single
    // 走 `feed`（clip 路内拿帧方式），legacy_* 走 clip+eager+render 组合
    // （blt 整推 / direct+bsp_only / direct+bsp_only+scalar），见 `docs/player.md`。
    let mut fs = Fs::new()?;
    let clip = load_video_clip(&mut fs, need_eager_clip())?;

    // 渲染路径矩阵（\player.toml，见 `Screen::ensure_render_calibrated`）：
    //   blt            → 单线程走 GOP Blt 引擎（workers 无意义）
    //   direct + bsp   → BSP 单核直写（MP 关闭或校准判单核更快）
    //   direct + all   → 原来的多核直写
    // 调度后端（[schedule] backend）：manual 走下面三路；nano 是单核执行器泵
    // （render 全 honoring）；dynamic 是多核 steal 泵（只直写前台，见 exec）。
    match crate::config::schedule_backend() {
        crate::config::ScheduleBackend::Nano => {
            log::info!(
                "play: path=nano_pump (single executor task; schedule workers/dist ignored)"
            );
            exec::nano_pump(screen, clip, VIDEO_W, VIDEO_H, target, tps)
        }
        crate::config::ScheduleBackend::Dynamic => {
            exec::dynamic_draw(screen, clip, VIDEO_W, VIDEO_H, target, tps)
        }
        crate::config::ScheduleBackend::Manual => manual_run(screen, clip, target, tps),
    }
}

/// 手写调度（`[schedule] backend=manual`，也是执行器后端的回退落点）：
/// blt 单线程引擎 / direct BSP 单核直写 / direct 全核 `mp_draw`，`workers`/`dist` 生效。
pub(crate) fn manual_run(screen: &mut Screen, clip: Clip, target: Option<u32>, tps: u64) -> Result {
    match crate::config::effective_render() {
        crate::config::RenderMode::Blt => {
            log::info!("play: path=blt_loop");
            blt_loop(screen, clip, VIDEO_W, VIDEO_H, target, tps)
        }
        _ => match crate::config::effective_workers() {
            crate::config::WorkersMode::BspOnly => {
                log::info!("play: path=direct_loop");
                direct_loop(screen, clip, VIDEO_W, VIDEO_H, target, tps)
            }
            _ => {
                log::info!("play: path=mp_draw");
                mp_draw(screen, clip, VIDEO_W, VIDEO_H, target, tps)
            }
        },
    }
}
