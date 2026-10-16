#![no_main]
#![no_std]

extern crate alloc;

mod config;
mod diag;
mod error;
mod fs;
mod graphics;
mod menu;
mod video;

use uefi::prelude::*;

use crate::graphics::Screen;
use crate::menu::run_menu;

#[entry]
fn main() -> Status {
    uefi::helpers::init().expect("Failed to init UEFI");

    // 关固件看门狗：UEFI 规范要求镜像加载后 5 分钟倒计时，到时未
    // ExitBootServices 就复位机器。菜单 8 全矩阵实测常超过 5 分钟
    // （QEMU HVF 下已复现无征兆复位），必须在入口立即禁用。
    // timeout=0 即禁用；部分固件无看门狗会报 UNSUPPORTED，忽略。
    if uefi::boot::set_watchdog_timer(0, 0x10000, None).is_err() {
        log::warn!("cfg: no firmware watchdog (or disable failed), continuing");
    }

    // 固件原始 XSAVE 状态必须在任何菜单项（可能自行 enable）之前采一次，
    // 否则报告会被工具自己的 enable 污染。
    diag::common::capture_firmware_osxsave();

    // 读加速矩阵配置（\palyer.toml）；缺失/非法 → 默认 + 一行警告。
    let cfg_warn = config::load();

    let mut screen = Screen::new().expect("Failed to create screen");
    if let Some(w) = cfg_warn {
        log::warn!("{}", w);
        screen.draw_str(&w);
    }

    // render.mode = "auto"：开机微基准在 直写 / Blt 之间选快的。
    if config::render_mode() == config::RenderMode::Auto {
        screen.bench_render(config::tune_frames());
    }

    // 默认停留等待：由菜单分发播放视频或各诊断项，不再直进 video_run。
    run_menu(screen);
}
