//! ptui 显示页：适配/锚点/黑边（只改 runtime 内存，不碰屏显管线）。
//!
//! - `f` 回绕 `display.fit`（stretch/fit/cover/scale_down/native），
//!   `a` 回绕 `display.align`（center/left/right/top/bottom），
//!   `b` 轮换黑边预设（black/white/gray/navy）。
//! - 数字 `1`-`3` 选中 R/G/B 通道，`+`/`-` 按 16 步进（0-255）。
//! - 每次调档双写 `ptui: set display.*=<v>`（bar 为小写 hex 无 `#`）；
//!   进页记 `ptui: page=display`。

use alloc::format;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 黑边预设（`b` 键轮换；hex 口径与 `BarColor::to_hex` 一致）。
const PRESETS: [(u8, u8, u8); 4] = [(0, 0, 0), (255, 255, 255), (128, 128, 128), (0, 0, 128)];

/// 显示页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Display));
    flush_keys();
    // 当前黑边落在哪个预设（不命中从 0 起轮）。
    let mut preset: usize = 0;
    let mut sel: usize = 0;
    loop {
        draw(&mut screen, sel);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            'f' => {
                let next = state::step_fit(crate::config::runtime().display_fit, 1);
                crate::config::runtime_mut().display_fit = next;
                state::set(&mut screen, "display.fit", next.file_str());
            }
            'a' => {
                let next = state::step_align(crate::config::runtime().display_align, 1);
                crate::config::runtime_mut().display_align = next;
                state::set(&mut screen, "display.align", next.file_str());
            }
            'b' => {
                preset = (preset + 1) % PRESETS.len();
                let (r, g, b) = PRESETS[preset];
                let rt = crate::config::runtime_mut();
                rt.display_bar = crate::config::BarColor { r, g, b };
                state::set(&mut screen, "display.bar", &hex_of(r, g, b));
            }
            d if ('1'..='3').contains(&d) => {
                sel = (d as u8 - b'1') as usize;
            }
            '+' | '=' => channel(&mut screen, sel, 16),
            '-' | '_' => channel(&mut screen, sel, -16),
            _ => {}
        }
    }
}

/// 显示页屏显（三开关行 + 通道行 + 按键约定行）。
fn draw(screen: &mut Screen, sel: usize) {
    let rt = crate::config::runtime();
    let (r, g, b) = (rt.display_bar.r, rt.display_bar.g, rt.display_bar.b);
    screen.draw_str("== display ==");
    screen.draw_str(&format!(
        "[f] fit={} (stretch/fit/cover/scale_down/native)",
        rt.display_fit.file_str()
    ));
    screen.draw_str(&format!(
        "[a] align={} (center/left/right/top/bottom)",
        rt.display_align.file_str()
    ));
    screen.draw_str(&format!("[b] bar={}", hex_of(r, g, b)));
    let names = ["R", "G", "B"];
    let vals = [r, g, b];
    for (i, (name, val)) in names.iter().zip(vals.iter()).enumerate() {
        let mark = if i == sel { ">" } else { " " };
        screen.draw_str(&format!("{} [{}] {}={} (0-255)", mark, i + 1, name, val));
    }
    screen.draw_str("[f/a/b] [1-3] [+/-] [q] 返回");
}

/// 单通道 ±16（0-255 钳制；只改 runtime 内存）。
fn channel(screen: &mut Screen, sel: usize, delta: i16) {
    let rt = crate::config::runtime_mut();
    let cur = [rt.display_bar.r, rt.display_bar.g, rt.display_bar.b];
    let v = (cur[sel] as i16 + delta).clamp(0, 255) as u8;
    let mut next = cur;
    next[sel] = v;
    rt.display_bar = crate::config::BarColor {
        r: next[0],
        g: next[1],
        b: next[2],
    };
    state::set(screen, "display.bar", &hex_of(next[0], next[1], next[2]));
}

/// 小写 hex（无 `#`，与 `BarColor::to_hex` 同形）。
fn hex_of(r: u8, g: u8, b: u8) -> alloc::string::String {
    format!("{:02x}{:02x}{:02x}", r, g, b)
}
