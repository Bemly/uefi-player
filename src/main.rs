#![no_main]
#![no_std]

extern crate alloc;

mod boot;
mod config;
mod diag;
mod error;
mod font;
mod fs;
mod graphics;
mod idle;
mod menu;
mod ptui;
mod video;
mod wallpaper;

use uefi::prelude::*;

use crate::graphics::Screen;

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

    // 读加速矩阵配置（\player.toml）；缺失/非法 → 默认 + 一行警告。
    let cfg_warn = config::load();

    // P2：探硬件最大搬运位宽（开 XSAVE + CPUID/XCR0 门控），之后所有上屏/
    // 播放按 `accel.level` × 硬件上限选路。必须在首次上屏前完成。
    config::resolve_accel();

    let mut screen = Screen::new().expect("Failed to create screen");
    if let Some(w) = cfg_warn {
        log::warn!("{}", w);
        screen.draw_str(&w);
    }

    // render auto 矩阵：首启必校准（direct/blt × BSP/全核 × vsync 探测），
    // 有记忆直接复用快启，结论回写 `\player.toml` 的 `measured_*`。
    // 这就是“首轮默认测试”：无校准结果不开机菜单，先跑矩阵。
    screen.ensure_render_calibrated();

    // 顶层交互：壁纸待机 → 任意键弹窗（引导项/播放器/固件设置/开关机）。
    // 纯播放器模式（旧开机菜单）只在弹窗按 `p` 才进。
    idle::run(screen);
}
