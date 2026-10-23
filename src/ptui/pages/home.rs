//! ptui 首页：九宫导航（播放/加速/渲染/调度/解码/显示/测试/壁纸 + 返回）。
//!
//! - 数字键嵌套进子页（`play`/`accel`/... 各自循环到 `q` 才回来），
//!   `q` 回外壳调用方（待机/弹窗页栈继续）。
//! - 进页记 `ptui: page=home`（`state::goto` 双写清屏）。

use core::time::Duration;

use uefi::boot::stall;

use crate::diag::common::{flush_keys, poll_key};
use crate::graphics::Screen;

use super::super::state;
use super::{accel, decode, display, play, render, schedule, tests, wallpaper};

/// 首页主循环（值进值出 `Screen`；`q` 返回外壳）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Home));
    flush_keys();
    loop {
        draw(&mut screen);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            '1' => screen = play::run(screen),
            '2' => screen = accel::run(screen),
            '3' => screen = render::run(screen),
            '4' => screen = schedule::run(screen),
            '5' => screen = decode::run(screen),
            '6' => screen = display::run(screen),
            '7' => screen = tests::run(screen),
            '8' => screen = wallpaper::run(screen),
            'q' => {
                log::info!("ptui: back to caller");
                return screen;
            }
            _ => {
                screen.draw_str("错误按键: 1-8 确认, q 返回");
                stall(Duration::from_millis(500));
            }
        }
        // 子页回来后重记首页 marker（矩阵看连续 page= 行可断言返回路径）。
        state::goto(&mut screen, state::page_name(state::Next::Home));
        flush_keys();
    }
}

/// 首页屏显（九宫 + 按键约定行；中文标签直写，字形由字体 agent 接管）。
fn draw(screen: &mut Screen) {
    screen.draw_str("== 播放器菜单 ==");
    screen.draw_str("[1] 播放");
    screen.draw_str("[2] accel");
    screen.draw_str("[3] render");
    screen.draw_str("[4] schedule");
    screen.draw_str("[5] decode");
    screen.draw_str("[6] display");
    screen.draw_str("[7] test");
    screen.draw_str("[8] 壁纸设置");
    screen.draw_str("[1-8] 确认  [q] 返回");
}

/// 阻塞等一个可打印键（各子页共用；`None` 不出现：`poll_key` 无键即 `None`，stall 轮询）。
pub fn wait_key() -> Option<(char, u16)> {
    loop {
        if let Some(k) = poll_key() {
            return Some(k);
        }
        stall(Duration::from_millis(50));
    }
}

/// 子页共用的阻塞等键（屏显-only 的确认/报错页用）。
pub fn wait_any(screen: &mut Screen) {
    screen.draw_str("按键继续");
    flush_keys();
    loop {
        if poll_key().is_some() {
            return;
        }
        stall(Duration::from_millis(50));
    }
}

/// 子页共用的屏显报错（串口 warn + 屏显 + 等键）。
pub fn show_err(screen: &mut Screen, msg: &str) {
    log::warn!("{}", msg);
    screen.draw_str(msg);
    wait_any(screen);
}
