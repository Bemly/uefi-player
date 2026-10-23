//! ptui 加速页：指令集档位（31 档回绕拨档，只改 runtime 内存）。
//!
//! - `+`/`n` 下一档，`-`/`p` 上一档（`auto` 恒首，按性能序，见 `config::types`）；
//!   档位串即落盘小写形（`sse42`/`avx2`/`avx512f`...）。
//! - 每次调档双写 `ptui: set accel.level=<str>`；进页记 `ptui: page=accel`。

use alloc::format;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 加速页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Accel));
    flush_keys();
    loop {
        draw(&mut screen);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            '+' | '=' | 'n' => step(&mut screen, 1),
            '-' | '_' | 'p' => step(&mut screen, -1),
            _ => {}
        }
    }
}

/// 加速页屏显（当前档 + 档位/前后预览 + 按键约定行）。
fn draw(screen: &mut Screen) {
    let cur = crate::config::runtime().accel_level;
    let i = cur.as_u8() as usize;
    screen.draw_str("== accel ==");
    screen.draw_str(&format!(
        "accel.level={} ({}/{})",
        state::ACCEL_STRS[i],
        i + 1,
        state::ACCEL_COUNT
    ));
    // 前后预览各两档（边界回绕，拨档手感连续）。
    let prev1 = state::step_accel(cur, -1);
    let prev2 = state::step_accel(cur, -2);
    let next1 = state::step_accel(cur, 1);
    let next2 = state::step_accel(cur, 2);
    screen.draw_str(&format!(
        "... {} <- {} <- [{}] -> {} -> {} ...",
        state::ACCEL_STRS[prev2.as_u8() as usize],
        state::ACCEL_STRS[prev1.as_u8() as usize],
        state::ACCEL_STRS[i],
        state::ACCEL_STRS[next1.as_u8() as usize],
        state::ACCEL_STRS[next2.as_u8() as usize],
    ));
    screen.draw_str("[+/-] (n/p)  [q] 返回");
}

/// 拨一档（只改 runtime 内存；`scalar`=关解码 SIMD，其余开，见 `resolve_accel`）。
fn step(screen: &mut Screen, delta: i8) {
    let next = state::step_accel(crate::config::runtime().accel_level, delta);
    crate::config::runtime_mut().accel_level = next;
    state::set(
        screen,
        "accel.level",
        state::ACCEL_STRS[next.as_u8() as usize],
    );
}
