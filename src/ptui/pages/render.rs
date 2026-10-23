//! ptui 渲染页：路径/vsync/引擎/微调数字（只改 runtime 内存）。
//!
//! - 面板八行：mode / vsync / blt_op 三开关 + frames/tile_rows/vsync_fps/buffers/window
//!   五微调；↑↓ 选行、←→ 拨（开关回绕、数字按步长）、Enter 同 →。
//! - 旧快捷键保留：`m`/`v`/`b` 回绕三开关，数字 `1`-`5` 选中微调行，`+`/`-` 调选中行
//!   （钳制口径与 `runtime::apply_cfg` 一致）。
//! - 每次调档记 `ptui: set render.*=<v>`；进页记 `ptui: page=render`。

use alloc::format;
use alloc::string::String;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

/// 行数：三开关 + 五微调。
const N: usize = 8;

/// 渲染页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Render));
    flush_keys();
    // 默认选中首个微调行（旧口径：`+`/`-` 进页即调 frames）。
    let mut sel: usize = 3;
    loop {
        draw(&mut screen, sel);
        match ui::read(&mut screen, Target::Page(Next::Render)) {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up => sel = ui::step_sel(sel, N, true),
            Act::Down => sel = ui::step_sel(sel, N, false),
            Act::Right | Act::Enter => adjust(&mut screen, sel, 1),
            Act::Left => adjust(&mut screen, sel, -1),
            Act::Key('m') => adjust(&mut screen, 0, 1),
            Act::Key('v') => adjust(&mut screen, 1, 1),
            Act::Key('b') => adjust(&mut screen, 2, 1),
            Act::Key(d @ '1'..='5') => sel = 3 + (d as u8 - b'1') as usize,
            Act::Key('+' | '=') => adjust(&mut screen, sel, 1),
            Act::Key('-' | '_') => adjust(&mut screen, sel, -1),
            _ => {}
        }
    }
}

/// 渲染页面板。
fn draw(screen: &mut Screen, sel: usize) {
    let rt = crate::config::runtime();
    let tune = |v: u32, range: &str| format!("{}  ({})", v, range);
    let rows = [
        Row::new(
            "m",
            "render.mode",
            String::from(state::RENDER_STRS[rt.render_mode.as_u8() as usize]),
        ),
        Row::new(
            "v",
            "render.vsync",
            String::from(state::VSYNC_STRS[rt.vsync_mode.as_u8() as usize]),
        ),
        Row::new(
            "b",
            "render.blt_op",
            String::from(state::BLT_STRS[state::blt_to_u8(rt.render_blt_op) as usize]),
        ),
        Row::new("1", "tune.frames", tune(rt.tune_frames, ">=1")),
        Row::new("2", "tune.tile_rows", tune(rt.tune_tile_rows, "8-512 步 8")),
        Row::new("3", "tune.vsync_fps", tune(rt.vsync_fps, "15-240 步 5")),
        Row::new("4", "tune.buffers", tune(rt.tune_buffers, "0-16")),
        Row::new("5", "tune.window", tune(rt.tune_window, "1-32")),
    ];
    ui::draw(
        screen,
        &View {
            title: "渲染 render",
            rows: &rows,
            sel: Some(sel),
            notes: &[],
            hint: "↑↓ 选择  ←→ 调整  m/v/b 开关  1-5 微调行  +- 步进  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

/// 调一行（开关回绕；微调步长：frames ±1 / tile_rows ±8 / vsync_fps ±5 /
/// buffers ±1 / window ±1；钳制口径与 `runtime::apply_cfg` 一致，只改 runtime 内存）。
fn adjust(screen: &mut Screen, sel: usize, delta: i64) {
    let rt = crate::config::runtime_mut();
    let d8 = delta as i8;
    match sel {
        0 => {
            let next = state::step_render(rt.render_mode, d8);
            rt.render_mode = next;
            state::set(
                screen,
                "render.mode",
                state::RENDER_STRS[next.as_u8() as usize],
            );
        }
        1 => {
            let next = state::step_vsync(rt.vsync_mode, d8);
            rt.vsync_mode = next;
            state::set(
                screen,
                "render.vsync",
                state::VSYNC_STRS[next.as_u8() as usize],
            );
        }
        2 => {
            let cur = state::blt_to_u8(rt.render_blt_op);
            let next = state::u8_to_blt(state::step_u8(cur, 4, d8));
            rt.render_blt_op = next;
            state::set(
                screen,
                "render.blt_op",
                state::BLT_STRS[state::blt_to_u8(next) as usize],
            );
        }
        3 => {
            let v = (rt.tune_frames as i64 + delta).max(1) as u32;
            rt.tune_frames = v;
            state::set(screen, "render.tune.frames", &format!("{}", v));
        }
        4 => {
            let v = (rt.tune_tile_rows as i64 + delta * 8).clamp(8, 512) as u32;
            rt.tune_tile_rows = v;
            state::set(screen, "render.tune.tile_rows", &format!("{}", v));
        }
        5 => {
            let v = (rt.vsync_fps as i64 + delta * 5).clamp(15, 240) as u32;
            rt.vsync_fps = v;
            state::set(screen, "render.tune.vsync_fps", &format!("{}", v));
        }
        6 => {
            let v = (rt.tune_buffers as i64 + delta).clamp(0, 16) as u32;
            rt.tune_buffers = v;
            state::set(screen, "render.tune.buffers", &format!("{}", v));
        }
        _ => {
            let v = (rt.tune_window as i64 + delta).clamp(1, 32) as u32;
            rt.tune_window = v;
            state::set(screen, "render.tune.window", &format!("{}", v));
        }
    }
}
