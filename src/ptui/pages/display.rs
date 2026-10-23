//! ptui 显示页：适配/锚点/黑边（只改 runtime 内存，不碰屏显管线）。
//!
//! - 面板六行：fit / align / bar 预设 / R / G / B；↑↓ 选行、←→ 拨、Enter 同 →。
//! - 旧快捷键保留：`f` 回绕 `display.fit`（stretch/fit/cover/scale_down/native），
//!   `a` 回绕 `display.align`（center/left/right/top/bottom），`b` 轮换黑边预设
//!   （black/white/gray/navy），数字 `1`-`3` 选 R/G/B，`+`/`-` 按 16 步进（0-255）。
//! - 每次调档记 `ptui: set display.*=<v>`（bar 为小写 hex 无 `#`）；
//!   进页记 `ptui: page=display`。

use alloc::format;
use alloc::string::String;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;
use uefi::proto::console::gop::BltPixel;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

/// 黑边预设（`b` 键轮换；hex 口径与 `BarColor::to_hex` 一致）。
const PRESETS: [(u8, u8, u8); 4] = [(0, 0, 0), (255, 255, 255), (128, 128, 128), (0, 0, 128)];
const N: usize = 6;

/// 显示页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Display));
    flush_keys();
    // 当前黑边落在哪个预设（不命中从 0 起轮）；默认选中 R 通道行（旧口径 `+`/`-` 调 R）。
    let mut preset: usize = 0;
    let mut sel: usize = 3;
    loop {
        draw(&mut screen, sel);
        match ui::read(&mut screen, Target::Page(Next::Display)) {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up => sel = ui::step_sel(sel, N, true),
            Act::Down => sel = ui::step_sel(sel, N, false),
            Act::Right | Act::Enter => adjust(&mut screen, sel, 1, &mut preset),
            Act::Left => adjust(&mut screen, sel, -1, &mut preset),
            Act::Key('f') => adjust(&mut screen, 0, 1, &mut preset),
            Act::Key('a') => adjust(&mut screen, 1, 1, &mut preset),
            Act::Key('b') => adjust(&mut screen, 2, 1, &mut preset),
            Act::Key(d @ '1'..='3') => sel = 3 + (d as u8 - b'1') as usize,
            Act::Key('+' | '=') if sel >= 3 => adjust(&mut screen, sel, 1, &mut preset),
            Act::Key('-' | '_') if sel >= 3 => adjust(&mut screen, sel, -1, &mut preset),
            _ => {}
        }
    }
}

/// 显示页面板（bar 行后附色块预览）。
fn draw(screen: &mut Screen, sel: usize) {
    let rt = crate::config::runtime();
    let (r, g, b) = (rt.display_bar.r, rt.display_bar.g, rt.display_bar.b);
    let rows = [
        Row::new(
            "f",
            "display.fit",
            format!(
                "{}  (stretch/fit/cover/scale_down/native)",
                rt.display_fit.file_str()
            ),
        ),
        Row::new(
            "a",
            "display.align",
            format!(
                "{}  (center/left/right/top/bottom)",
                rt.display_align.file_str()
            ),
        ),
        Row::new(
            "b",
            "display.bar",
            format!("#{}  (预设轮换)", hex_of(r, g, b)),
        ),
        Row::new("1", "  R", format!("{}  (0-255 步 16)", r)),
        Row::new("2", "  G", format!("{}  (0-255 步 16)", g)),
        Row::new("3", "  B", format!("{}  (0-255 步 16)", b)),
    ];
    let notes = [String::from("黑边色预览 ▶")];
    ui::draw(
        screen,
        &View {
            title: "显示 display",
            rows: &rows,
            sel: Some(sel),
            notes: &notes,
            hint: "↑↓ 选择  ←→ 调整  f/a/b 开关  1-3 通道  +- 步进  Tab 跳转  q 返回",
            compact: false,
        },
    );
    // 预览色块紧跟备注行（备注行 y = 面板底 + 8）。
    let y = ui::TOP + rows.len() * ui::ROW_H + 16 + 8;
    screen.fill_rect(ui::PAD_X + 120, y, 48, 16, BltPixel::new(r, g, b));
    ui::frame(screen, ui::PAD_X + 120, y, 48, 16, 1, ui::BORDER);
}

/// 调一行（fit/align 回绕；预设轮换；单通道 ±16 钳 0-255；只改 runtime 内存）。
fn adjust(screen: &mut Screen, sel: usize, delta: i8, preset: &mut usize) {
    let rt = crate::config::runtime_mut();
    match sel {
        0 => {
            let next = state::step_fit(rt.display_fit, delta);
            rt.display_fit = next;
            state::set(screen, "display.fit", next.file_str());
        }
        1 => {
            let next = state::step_align(rt.display_align, delta);
            rt.display_align = next;
            state::set(screen, "display.align", next.file_str());
        }
        2 => {
            *preset = state::step_u8(*preset as u8, PRESETS.len() as u8, delta) as usize;
            let (r, g, b) = PRESETS[*preset];
            rt.display_bar = crate::config::BarColor { r, g, b };
            state::set(screen, "display.bar", &hex_of(r, g, b));
        }
        _ => {
            let ch = sel - 3;
            let cur = [rt.display_bar.r, rt.display_bar.g, rt.display_bar.b];
            let v = (cur[ch] as i16 + delta as i16 * 16).clamp(0, 255) as u8;
            let mut next = cur;
            next[ch] = v;
            rt.display_bar = crate::config::BarColor {
                r: next[0],
                g: next[1],
                b: next[2],
            };
            state::set(screen, "display.bar", &hex_of(next[0], next[1], next[2]));
        }
    }
}

/// 小写 hex（无 `#`，与 `BarColor::to_hex` 同形）。
fn hex_of(r: u8, g: u8, b: u8) -> String {
    format!("{:02x}{:02x}{:02x}", r, g, b)
}
