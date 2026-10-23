//! ptui 壁纸页：内存调档 + 显式保存（只写 `[wallpaper]` 表）。
//!
//! - 面板五行：enable / fps / preload / popup_freeze / 保存；↑↓ 选行、←→ 拨、
//!   Enter 同 →（停在“保存”行即落盘）。
//! - 旧快捷键保留（只改内存）：`e` 切 enable，`f` 轮换 fps（auto/max/15/30/60/120），
//!   `+`/`-` 调 preload（0-32，0=不封顶），`z` 切 popup_freeze；
//!   每次调档记 `ptui: set wallpaper.*=<v>`。
//! - `s` 落盘：`config::save_wallpaper()`（只写 `[wallpaper]` 表四键，
//!   其余表重读原样保留），成功/失败显示在底栏状态。
//! - 进页记 `ptui: page=wallpaper`。

use alloc::format;
use alloc::string::String;

use crate::config::FpsPolicy;
use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

const N: usize = 5;

/// 壁纸页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Wallpaper));
    flush_keys();
    let mut sel: usize = 0;
    loop {
        draw(&mut screen, sel);
        match ui::read(&mut screen, Target::Page(Next::Wallpaper)) {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up => sel = ui::step_sel(sel, N, true),
            Act::Down => sel = ui::step_sel(sel, N, false),
            Act::Right | Act::Enter => adjust(&mut screen, sel, 1),
            Act::Left => adjust(&mut screen, sel, -1),
            Act::Key('e') => adjust(&mut screen, 0, 1),
            Act::Key('f') => adjust(&mut screen, 1, 1),
            Act::Key('+' | '=') => adjust(&mut screen, 2, 1),
            Act::Key('-' | '_') => adjust(&mut screen, 2, -1),
            Act::Key('z') => adjust(&mut screen, 3, 1),
            Act::Key('s') => adjust(&mut screen, 4, 1),
            _ => {}
        }
    }
}

/// 壁纸页面板。
fn draw(screen: &mut Screen, sel: usize) {
    let rows = [
        Row::new(
            "e",
            "wallpaper.enable",
            String::from(state::onoff(crate::config::wallpaper_enabled())),
        ),
        Row::new(
            "f",
            "wallpaper.fps",
            format!(
                "{}  (auto/max/15/30/60/120)",
                fps_str(crate::config::wallpaper_fps_policy())
            ),
        ),
        Row::new(
            "+-",
            "wallpaper.preload",
            format!("{}  (0 = 不封顶)", crate::config::wallpaper_preload()),
        ),
        Row::new(
            "z",
            "wallpaper.popup_freeze",
            String::from(state::onoff(crate::config::wallpaper_popup_freeze())),
        ),
        Row::new(
            "s",
            "保存到 player.toml",
            String::from("只写 [wallpaper] 表"),
        ),
    ];
    let notes = [String::from(
        "画廊片源由 [playlist] 决定（host 配置 GUI 编辑）",
    )];
    ui::draw(
        screen,
        &View {
            title: "壁纸设置",
            rows: &rows,
            sel: Some(sel),
            notes: &notes,
            hint: "↑↓ 选择  ←→ 调整  e/f/z 开关  +- 预载  s 保存  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

/// 调一行（只改内存；保存行落盘）。
fn adjust(screen: &mut Screen, sel: usize, delta: i64) {
    match sel {
        0 => {
            let next = !crate::config::wallpaper_enabled();
            crate::config::set_wallpaper_enable(next);
            state::set(screen, "wallpaper.enable", state::onoff(next));
        }
        1 => {
            // fps 轮换（FPS_SEQ 序；只改内存，落盘按 `s`）。
            let cur = fps_str(crate::config::wallpaper_fps_policy());
            let idx = state::FPS_SEQ.iter().position(|&s| s == cur).unwrap_or(0);
            let n = state::FPS_SEQ.len() as i64;
            let next = state::FPS_SEQ[(idx as i64 + delta).rem_euclid(n) as usize];
            crate::config::set_wallpaper_fps(next);
            state::set(screen, "wallpaper.fps", next);
        }
        2 => {
            // preload ±1（已钳 0-32；`set_wallpaper_preload` 内复钳）。
            let v = (crate::config::wallpaper_preload() as i64 + delta).clamp(0, 32) as u32;
            crate::config::set_wallpaper_preload(v);
            state::set(screen, "wallpaper.preload", &format!("{}", v));
        }
        3 => {
            let next = !crate::config::wallpaper_popup_freeze();
            crate::config::set_wallpaper_popup_freeze(next);
            state::set(screen, "wallpaper.popup_freeze", state::onoff(next));
        }
        _ => match crate::config::save_wallpaper() {
            Ok(()) => {
                log::info!("ptui: wallpaper saved ([wallpaper] only)");
                ui::set_status(String::from("已保存 [wallpaper]"));
            }
            Err(e) => {
                log::warn!("ptui: wallpaper save failed: {:?}", e);
                ui::set_status(format!("壁纸保存失败: {:?}", e));
            }
        },
    }
}

/// 壁纸 fps 策略转文件串（与 `save_wallpaper` 往返同形）。
fn fps_str(p: FpsPolicy) -> String {
    match p {
        FpsPolicy::Auto => String::from("auto"),
        FpsPolicy::Max => String::from("max"),
        FpsPolicy::Capped(n) => format!("{}", n),
    }
}
