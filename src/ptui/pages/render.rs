//! ptui 渲染页：路径/vsync/引擎/微调数字（只改 runtime 内存）。
//!
//! - `m` 回绕 `render.mode`（auto/direct/blt），`v` 回绕 `render.vsync`
//!   （auto/static/off/half），`b` 回绕 `render.blt_op`（四引擎动作）。
//! - 数字 `1`-`5` 选中微调行（frames/tile_rows/vsync_fps/buffers/window），
//!   `+`/`-` 按步长调（钳制口径与 `runtime::apply_cfg` 一致）。
//! - 每次调档双写 `ptui: set render.*=<v>`；进页记 `ptui: page=render`。

use alloc::format;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 微调行下标 0-4 → frames/tile_rows/vsync_fps/buffers/window。
/// 渲染页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Render));
    flush_keys();
    let mut sel: usize = 0;
    loop {
        draw(&mut screen, sel);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            'm' => {
                let next = state::step_render(crate::config::runtime().render_mode, 1);
                crate::config::runtime_mut().render_mode = next;
                state::set(
                    &mut screen,
                    "render.mode",
                    state::RENDER_STRS[next.as_u8() as usize],
                );
            }
            'v' => {
                let next = state::step_vsync(crate::config::runtime().vsync_mode, 1);
                crate::config::runtime_mut().vsync_mode = next;
                state::set(
                    &mut screen,
                    "render.vsync",
                    state::VSYNC_STRS[next.as_u8() as usize],
                );
            }
            'b' => {
                let cur = state::blt_to_u8(crate::config::runtime().render_blt_op);
                let next = state::u8_to_blt(state::step_u8(cur, 4, 1));
                crate::config::runtime_mut().render_blt_op = next;
                state::set(
                    &mut screen,
                    "render.blt_op",
                    state::BLT_STRS[state::blt_to_u8(next) as usize],
                );
            }
            d if ('1'..='5').contains(&d) => {
                sel = (d as u8 - b'1') as usize;
            }
            '+' | '=' => tune(&mut screen, sel, 1),
            '-' | '_' => tune(&mut screen, sel, -1),
            _ => {}
        }
    }
}

/// 渲染页屏显（三开关行 + 五微调行 + 按键约定行）。
fn draw(screen: &mut Screen, sel: usize) {
    let rt = crate::config::runtime();
    screen.draw_str("== render ==");
    screen.draw_str(&format!(
        "[m] mode={}  [v] vsync={}  [b] blt_op={}",
        state::RENDER_STRS[rt.render_mode.as_u8() as usize],
        state::VSYNC_STRS[rt.vsync_mode.as_u8() as usize],
        state::BLT_STRS[state::blt_to_u8(rt.render_blt_op) as usize],
    ));
    let rows = [
        ("frames", rt.tune_frames, ">=1"),
        ("tile_rows", rt.tune_tile_rows, "8-512"),
        ("vsync_fps", rt.vsync_fps, "15-240"),
        ("buffers", rt.tune_buffers, "0-16"),
        ("window", rt.tune_window, "1-32"),
    ];
    for (i, (name, val, range)) in rows.iter().enumerate() {
        let mark = if i == sel { ">" } else { " " };
        screen.draw_str(&format!(
            "{} [{}] render.tune.{}={} ({})",
            mark,
            i + 1,
            name,
            val,
            range
        ));
    }
    screen.draw_str("[m/v/b] [1-5] [+/-] [q] 返回");
}

/// 微调一行（步长：frames ±1 / tile_rows ±8 / vsync_fps ±5 / buffers ±1 / window ±1；
/// 钳制口径与 `runtime::apply_cfg` 一致，只改 runtime 内存）。
fn tune(screen: &mut Screen, sel: usize, delta: i64) {
    let rt = crate::config::runtime_mut();
    match sel {
        0 => {
            let v = (rt.tune_frames as i64 + delta).max(1) as u32;
            rt.tune_frames = v;
            state::set(screen, "render.tune.frames", &format!("{}", v));
        }
        1 => {
            let v = (rt.tune_tile_rows as i64 + delta * 8).clamp(8, 512) as u32;
            rt.tune_tile_rows = v;
            state::set(screen, "render.tune.tile_rows", &format!("{}", v));
        }
        2 => {
            let v = (rt.vsync_fps as i64 + delta * 5).clamp(15, 240) as u32;
            rt.vsync_fps = v;
            state::set(screen, "render.tune.vsync_fps", &format!("{}", v));
        }
        3 => {
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
