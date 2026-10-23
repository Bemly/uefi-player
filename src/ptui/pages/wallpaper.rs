//! ptui 壁纸页：内存调档 + 显式保存（只写 `[wallpaper]` 表）。
//!
//! - 调档经契约写口（只改内存）：`e` 切 enable，`f` 轮换 fps
//!  （auto/max/15/30/60/120），`+`/`-` 调 preload（0-32，0=不封顶），
//!   `z` 切 popup_freeze；每次调档双写 `ptui: set wallpaper.*=<v>`。
//! - `s` 落盘：`config::save_wallpaper()`（只写 `[wallpaper]` 表四键，
//!   其余表重读原样保留），成功/失败屏显一行。
//! - 进页记 `ptui: page=wallpaper`。

use alloc::format;

use crate::config::FpsPolicy;
use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 壁纸页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Wallpaper));
    flush_keys();
    loop {
        draw(&mut screen);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            'e' => {
                let next = !crate::config::wallpaper_enabled();
                crate::config::set_wallpaper_enable(next);
                state::set(&mut screen, "wallpaper.enable", state::onoff(next));
            }
            'f' => cycle_fps(&mut screen),
            '+' | '=' => preload(&mut screen, 1),
            '-' | '_' => preload(&mut screen, -1),
            'z' => {
                let next = !crate::config::wallpaper_popup_freeze();
                crate::config::set_wallpaper_popup_freeze(next);
                state::set(&mut screen, "wallpaper.popup_freeze", state::onoff(next));
            }
            's' => match crate::config::save_wallpaper() {
                Ok(()) => {
                    log::info!("ptui: wallpaper saved ([wallpaper] only)");
                    screen.draw_str("壁纸设置确认 [wallpaper]");
                }
                Err(e) => {
                    screen.draw_str(&format!("壁纸错误: {:?}", e));
                }
            },
            _ => {}
        }
    }
}

/// 壁纸页屏显（四键行 + 保存行 + 按键约定行）。
fn draw(screen: &mut Screen) {
    screen.draw_str("== 壁纸 ==");
    screen.draw_str(&format!(
        "[e] enable={}",
        state::onoff(crate::config::wallpaper_enabled())
    ));
    screen.draw_str(&format!(
        "[f] fps={} (auto/max/15/30/60/120)",
        fps_str(crate::config::wallpaper_fps_policy())
    ));
    screen.draw_str(&format!(
        "[+/-] preload={} (0: no limit)",
        crate::config::wallpaper_preload()
    ));
    screen.draw_str(&format!(
        "[z] popup_freeze={}",
        state::onoff(crate::config::wallpaper_popup_freeze())
    ));
    screen.draw_str("[s] save [wallpaper]  [q] 返回");
}

/// `f`：轮换壁纸 fps（FPS_SEQ 序；只改内存，落盘按 `s`）。
fn cycle_fps(screen: &mut Screen) {
    let cur = fps_str(crate::config::wallpaper_fps_policy());
    let mut idx = state::FPS_SEQ.iter().position(|&s| s == cur).unwrap_or(0);
    idx = (idx + 1) % state::FPS_SEQ.len();
    let next = state::FPS_SEQ[idx];
    crate::config::set_wallpaper_fps(next);
    state::set(screen, "wallpaper.fps", next);
}

/// preload ±1（只改内存，已钳 0-32；`set_wallpaper_preload` 内复钳）。
fn preload(screen: &mut Screen, delta: i64) {
    let v = (crate::config::wallpaper_preload() as i64 + delta).clamp(0, 32) as u32;
    crate::config::set_wallpaper_preload(v);
    state::set(screen, "wallpaper.preload", &format!("{}", v));
}

/// 壁纸 fps 策略转文件串（与 `save_wallpaper` 往返同形）。
fn fps_str(p: FpsPolicy) -> alloc::string::String {
    match p {
        FpsPolicy::Auto => alloc::string::String::from("auto"),
        FpsPolicy::Max => alloc::string::String::from("max"),
        FpsPolicy::Capped(n) => format!("{}", n),
    }
}
