//! ptui 解码页：八格式线程/零拷/流式/缓冲/供片（只改 runtime 内存）。
//!
//! - 数字 `1`-`8` 选中格式（qois/raw/hevc/vp9/h263/mpeg2v/theora/avc），
//!   `t` 拨 threads（bsp_only/all），`z` 切 zero_copy，`s` 切 streaming，
//!   `f` 轮换 fetch（inherit + 五档 feed），`+`/`-` 调 buffers（0-32，0=跟随）。
//! - raw 几何（width/height）不在此调（盘上 `[decode.raw]` 只读，见回报）。
//! - 每次调档双写 `ptui: set decode.<fmt>.*=<v>`；进页记 `ptui: page=decode`。

use alloc::format;

use crate::config::{DecodeEntry, FeedMode, PlayerRuntime};
use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 解码页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Decode));
    flush_keys();
    let mut sel: usize = 0;
    loop {
        draw(&mut screen, sel);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            d if ('1'..='8').contains(&d) => {
                sel = (d as u8 - b'1') as usize;
            }
            't' => {
                let cur = entry(crate::config::runtime(), sel).threads;
                let next = state::step_threads(cur);
                entry_mut(crate::config::runtime_mut(), sel).threads = next;
                state::set(
                    &mut screen,
                    &format!("decode.{}.threads", state::FMT_KEYS[sel]),
                    state::threads_str(next),
                );
            }
            'z' => {
                let cur = entry(crate::config::runtime(), sel).zero_copy;
                entry_mut(crate::config::runtime_mut(), sel).zero_copy = !cur;
                state::set(
                    &mut screen,
                    &format!("decode.{}.zero_copy", state::FMT_KEYS[sel]),
                    state::onoff(!cur),
                );
            }
            's' => {
                let cur = entry(crate::config::runtime(), sel).streaming;
                entry_mut(crate::config::runtime_mut(), sel).streaming = !cur;
                state::set(
                    &mut screen,
                    &format!("decode.{}.streaming", state::FMT_KEYS[sel]),
                    state::onoff(!cur),
                );
            }
            'f' => {
                let cur = entry(crate::config::runtime(), sel).fetch;
                let next = step_fetch(cur);
                entry_mut(crate::config::runtime_mut(), sel).fetch = next;
                state::set(
                    &mut screen,
                    &format!("decode.{}.fetch", state::FMT_KEYS[sel]),
                    fetch_str(next),
                );
            }
            '+' | '=' => buffers(&mut screen, sel, 1),
            '-' | '_' => buffers(&mut screen, sel, -1),
            _ => {}
        }
    }
}

/// 解码页屏显（八格式一行摘要 + 选中行明细 + 按键约定行）。
fn draw(screen: &mut Screen, sel: usize) {
    screen.draw_str("== decode ==");
    for (i, key) in state::FMT_KEYS.iter().enumerate() {
        let e = entry(crate::config::runtime(), i);
        let mark = if i == sel { ">" } else { " " };
        screen.draw_str(&format!(
            "{} [{}] {} threads={} buffers={} fetch={}",
            mark,
            i + 1,
            key,
            state::threads_str(e.threads),
            e.buffers,
            fetch_str(e.fetch),
        ));
    }
    let e = entry(crate::config::runtime(), sel);
    screen.draw_str(&format!(
        "> {}: zero_copy={} streaming={}",
        state::FMT_KEYS[sel],
        state::onoff(e.zero_copy),
        state::onoff(e.streaming),
    ));
    screen.draw_str("[1-8] [t/z/s/f] [+/-] [q] 返回");
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

/// fetch 六态轮换（inherit → 五档 feed → inherit）。
fn step_fetch(cur: Option<FeedMode>) -> Option<FeedMode> {
    let idx = match cur {
        None => 0,
        Some(f) => f.as_u8() + 1,
    };
    let next = (idx + 1) % 6;
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
