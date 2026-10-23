//! ptui 加速页：指令集档位（31 档回绕拨档，只改 runtime 内存）。
//!
//! - ←/→（或 `-`/`p`、`+`/`n`）拨档，Enter 下一档（`auto` 恒首，按性能序，
//!   见 `config::types`）；档位串即落盘小写形（`sse42`/`avx2`/`avx512f`...）。
//! - 每次调档记 `ptui: set accel.level=<str>`；进页记 `ptui: page=accel`。

use alloc::format;
use alloc::string::String;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

/// 加速页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Accel));
    flush_keys();
    loop {
        draw(&mut screen);
        match ui::read(&mut screen, Target::Page(Next::Accel)) {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Right | Act::Enter => step(&mut screen, 1),
            Act::Left => step(&mut screen, -1),
            Act::Key('+' | '=' | 'n') => step(&mut screen, 1),
            Act::Key('-' | '_' | 'p') => step(&mut screen, -1),
            _ => {}
        }
    }
}

/// 加速页面板（当前档 + 前后各两档预览）。
fn draw(screen: &mut Screen) {
    let cur = crate::config::runtime().accel_level;
    let i = cur.as_u8() as usize;
    let name = |d: i8| state::ACCEL_STRS[state::step_accel(cur, d).as_u8() as usize];
    let rows = [Row::new(
        "←→",
        "accel.level",
        format!(
            "{}  ({}/{})",
            state::ACCEL_STRS[i],
            i + 1,
            state::ACCEL_COUNT
        ),
    )];
    let notes = [
        format!(
            "… {} ← {} ← [{}] → {} → {} …",
            name(-2),
            name(-1),
            state::ACCEL_STRS[i],
            name(1),
            name(2)
        ),
        String::from("scalar = 关解码 SIMD，其余档开（显式超硬件会被钳住）"),
    ];
    ui::draw(
        screen,
        &View {
            title: "加速 accel",
            rows: &rows,
            sel: Some(0),
            notes: &notes,
            hint: "←→ / +- 拨档  Enter 下一档  Tab 跳转  q 返回",
            compact: false,
        },
    );
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
