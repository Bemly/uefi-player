//! ptui 解码页：八格式线程/零拷/流式/缓冲/供片（只改 runtime 内存）。
//!
//! - 面板上 8 行是格式摘要（qois/raw/hevc/vp9/h263/mpeg2v/theora/avc），下 5 行是
//!   当前格式的明细（threads/zero_copy/streaming/fetch/buffers）；↑↓ 在 13 行间移动，
//!   停在格式行即切当前格式，停在明细行 ←→ 调该项、Enter 同 →。
//! - 旧快捷键保留：数字 `1`-`8` 选格式，`t` 拨 threads（bsp_only/all），`z` 切
//!   zero_copy，`s` 切 streaming，`f` 轮换 fetch（inherit + 五档 feed），`+`/`-` 调
//!   buffers（0-32，0=跟随）。
//! - raw 几何（width/height）不在此调（盘上 `[decode.raw]` 只读，见回报）。
//! - 每次调档记 `ptui: set decode.<fmt>.*=<v>`；进页记 `ptui: page=decode`。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{DecodeEntry, FeedMode, PlayerRuntime};
use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

/// 格式数、明细项数。
const NF: usize = 8;
const ND: usize = 5;

/// 解码页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Decode));
    flush_keys();
    // `fmt` 当前格式，`row` 面板光标（0-7 格式行，8-12 明细行）。
    let mut fmt: usize = 0;
    let mut row: usize = 0;
    loop {
        draw(&mut screen, fmt, row);
        let act = ui::read(&mut screen, Target::Page(Next::Decode));
        match act {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up | Act::Down => {
                row = ui::step_sel(row, NF + ND, matches!(act, Act::Up));
                if row < NF {
                    fmt = row;
                }
            }
            Act::Right | Act::Enter if row >= NF => detail(&mut screen, fmt, row - NF, 1),
            Act::Left if row >= NF => detail(&mut screen, fmt, row - NF, -1),
            Act::Key(d @ '1'..='8') => {
                fmt = (d as u8 - b'1') as usize;
                row = fmt;
            }
            Act::Key('t') => detail(&mut screen, fmt, 0, 1),
            Act::Key('z') => detail(&mut screen, fmt, 1, 1),
            Act::Key('s') => detail(&mut screen, fmt, 2, 1),
            Act::Key('f') => detail(&mut screen, fmt, 3, 1),
            Act::Key('+' | '=') => detail(&mut screen, fmt, 4, 1),
            Act::Key('-' | '_') => detail(&mut screen, fmt, 4, -1),
            _ => {}
        }
    }
}

/// 解码页面板（八格式摘要 + 当前格式明细）。
fn draw(screen: &mut Screen, fmt: usize, row: usize) {
    let rt = crate::config::runtime();
    const KEYS: [&str; NF] = ["1", "2", "3", "4", "5", "6", "7", "8"];
    let mut rows: Vec<Row> = Vec::with_capacity(NF + ND);
    for (i, key) in state::FMT_KEYS.iter().enumerate() {
        let e = entry(rt, i);
        let mark = if i == fmt { "● " } else { "" };
        rows.push(Row::new(
            KEYS[i],
            key,
            format!(
                "{}threads={} buffers={} fetch={}",
                mark,
                state::threads_str(e.threads),
                e.buffers,
                fetch_str(e.fetch),
            ),
        ));
    }
    let e = entry(rt, fmt);
    let name = state::FMT_KEYS[fmt];
    rows.push(Row::new(
        "t",
        "  threads",
        detail_val(name, state::threads_str(e.threads)),
    ));
    rows.push(Row::new(
        "z",
        "  zero_copy",
        detail_val(name, state::onoff(e.zero_copy)),
    ));
    rows.push(Row::new(
        "s",
        "  streaming",
        detail_val(name, state::onoff(e.streaming)),
    ));
    rows.push(Row::new(
        "f",
        "  fetch",
        detail_val(name, fetch_str(e.fetch)),
    ));
    rows.push(Row::new(
        "+-",
        "  buffers",
        format!("{}  ({}; 0 = 跟随 tune.window)", e.buffers, name),
    ));
    ui::draw(
        screen,
        &View {
            title: "解码 decode",
            rows: &rows,
            sel: Some(row),
            notes: &[],
            hint: "↑↓ 选择  ←→ 调整  1-8 格式  t/z/s/f/+- 明细  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

fn detail_val(fmt: &str, v: &str) -> String {
    format!("{}  ({})", v, fmt)
}

/// 调当前格式的一项明细（0 threads / 1 zero_copy / 2 streaming / 3 fetch / 4 buffers）。
fn detail(screen: &mut Screen, sel: usize, item: usize, delta: i64) {
    let key = state::FMT_KEYS[sel];
    match item {
        0 => {
            let next = state::step_threads(entry(crate::config::runtime(), sel).threads);
            entry_mut(crate::config::runtime_mut(), sel).threads = next;
            let k = format!("decode.{}.threads", key);
            state::set(screen, &k, state::threads_str(next));
        }
        1 => {
            let cur = entry(crate::config::runtime(), sel).zero_copy;
            entry_mut(crate::config::runtime_mut(), sel).zero_copy = !cur;
            let k = format!("decode.{}.zero_copy", key);
            state::set(screen, &k, state::onoff(!cur));
        }
        2 => {
            let cur = entry(crate::config::runtime(), sel).streaming;
            entry_mut(crate::config::runtime_mut(), sel).streaming = !cur;
            let k = format!("decode.{}.streaming", key);
            state::set(screen, &k, state::onoff(!cur));
        }
        3 => {
            let cur = entry(crate::config::runtime(), sel).fetch;
            let next = step_fetch(cur, delta);
            entry_mut(crate::config::runtime_mut(), sel).fetch = next;
            let k = format!("decode.{}.fetch", key);
            state::set(screen, &k, fetch_str(next));
        }
        _ => buffers(screen, sel, delta),
    }
}

/// buffers ±1（0-32，0=跟随 `tune.window`；只改 runtime 内存）。
fn buffers(screen: &mut Screen, sel: usize, delta: i64) {
    let e = entry_mut(crate::config::runtime_mut(), sel);
    let v = (e.buffers as i64 + delta).clamp(0, 32) as u32;
    e.buffers = v;
    state::set(
        screen,
        &format!("decode.{}.buffers", state::FMT_KEYS[sel]),
        &format!("{}", v),
    );
}

/// fetch 六态回绕（inherit ↔ 五档 feed）。
fn step_fetch(cur: Option<FeedMode>, delta: i64) -> Option<FeedMode> {
    let idx = match cur {
        None => 0,
        Some(f) => f.as_u8() + 1,
    };
    let next = (idx as i64 + delta).rem_euclid(6) as u8;
    if next == 0 {
        None
    } else {
        Some(FeedMode::from_u8(next - 1))
    }
}

/// fetch 文件串（`inherit` 或 feed 小写）。
fn fetch_str(f: Option<FeedMode>) -> &'static str {
    match f {
        None => state::FETCH_STRS[0],
        Some(v) => state::FETCH_STRS[v.as_u8() as usize + 1],
    }
}

/// 按下标读解码项（`DecodeEntry` 为 `Copy`，值返回）。
fn entry(rt: &PlayerRuntime, i: usize) -> DecodeEntry {
    match i {
        0 => rt.decode.qois,
        1 => rt.decode.raw,
        2 => rt.decode.hevc,
        3 => rt.decode.vp9,
        4 => rt.decode.h263,
        5 => rt.decode.mpeg2v,
        6 => rt.decode.theora,
        _ => rt.decode.avc,
    }
}

/// 按下标写解码项。
fn entry_mut(rt: &mut PlayerRuntime, i: usize) -> &mut DecodeEntry {
    match i {
        0 => &mut rt.decode.qois,
        1 => &mut rt.decode.raw,
        2 => &mut rt.decode.hevc,
        3 => &mut rt.decode.vp9,
        4 => &mut rt.decode.h263,
        5 => &mut rt.decode.mpeg2v,
        6 => &mut rt.decode.theora,
        _ => &mut rt.decode.avc,
    }
}
