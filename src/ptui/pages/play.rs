//! ptui 播放页：媒体扫描列表（数字选进即播）+ 播放档调档。
//!
//! - 列表来自 `media::scan()`（`config::media_dir` 单层目录，`""`=根）；
//!   显示前 10 项（数字键 `1`-`9`/`0` 选第 1-10 项），`r` 重扫，超 10 项只播前 10
//!   （媒体目录是人工摆片源的，列表短；矩阵用小目录即覆盖全量）。
//! - 起播一律走外壳在 `video::play` 导出的 `play_path`
//!  （值进值出 `Screen`；`Err` 时屏已释放，重建后报错显示，
//!   与外壳 `play_selected` 同式）。
//! - 调档（只改 runtime 内存）：`f` 轮换 `play_fps`（auto/max/15/30/60/120），
//!   `e` 回绕 `play_feed`（clip/staged/stream/zero_copy/single）；
//!   每次调档双写 `ptui: set play.fps=<v>` / `ptui: set play.feed=<v>`。
//! - 进页记 `ptui: page=play`；起播记 `ptui: play <i> path=<p>`。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use uefi::boot::stall;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::{media, state};
use super::home::{show_err, wait_key};

/// 播放页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Play));
    flush_keys();
    let mut list: Vec<String> = Vec::new();
    let mut err: Option<String> = None;
    match media::scan() {
        Ok(v) => list = v,
        Err(e) => err = Some(format!("错误: {:?}", e)),
    }
    loop {
        draw(&mut screen, &list, err.as_deref());
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            'r' => match media::scan() {
                Ok(v) => {
                    list = v;
                    err = None;
                }
                Err(e) => err = Some(format!("错误: {:?}", e)),
            },
            'f' => cycle_fps(&mut screen),
            'e' => cycle_feed(&mut screen),
            d if d.is_ascii_digit() => {
                let i = if d == '0' {
                    9
                } else {
                    (d as u8 - b'1') as usize
                };
                if i < list.len().min(10) {
                    screen = play_index(screen, &list[i], i);
                    state::goto(&mut screen, state::page_name(state::Next::Play));
                    flush_keys();
                }
            }
            _ => {}
        }
    }
}

/// 播放页屏显（当前 fps/feed 档 + 列表前 10 + 按键约定行）。
fn draw(screen: &mut Screen, list: &[String], err: Option<&str>) {
    let _ = screen.clear();
    screen.reset_cursor();
    screen.draw_str("== 播放 ==");
    screen.draw_str(&format!(
        "fps={} feed={}",
        crate::config::runtime().play_fps,
        state::FEED_STRS[crate::config::runtime().play_feed.as_u8() as usize]
    ));
    if let Some(e) = err {
        screen.draw_str(e);
    } else if list.is_empty() {
        screen.draw_str("empty: [r] rescan");
    } else {
        let show = list.len().min(10);
        for (i, name) in list.iter().take(show).enumerate() {
            let key = if i == 9 {
                '0'
            } else {
                (b'1' + i as u8) as char
            };
            screen.draw_str(&format!("[{}] {}", key, name));
        }
        if list.len() > show {
            screen.draw_str(&format!("... {} files (top 10)", list.len()));
        }
    }
    screen.draw_str("[0-9] 播放 [f] fps [e] feed [r] rescan [q] 返回");
}

/// `f`：轮换 `play_fps`（FPS_SEQ 序；只改 runtime 内存）。
fn cycle_fps(screen: &mut Screen) {
    let cur = crate::config::runtime().play_fps.clone();
    let mut idx = state::FPS_SEQ.iter().position(|&s| s == cur).unwrap_or(0);
    idx = (idx + 1) % state::FPS_SEQ.len();
    let next = state::FPS_SEQ[idx];
    crate::config::runtime_mut().play_fps = String::from(next);
    state::set(screen, "play.fps", next);
}

/// `e`：回绕 `play_feed`（只改 runtime 内存）。
fn cycle_feed(screen: &mut Screen) {
    let next = state::step_feed(crate::config::runtime().play_feed, 1);
    crate::config::runtime_mut().play_feed = next;
    state::set(screen, "play.feed", state::FEED_STRS[next.as_u8() as usize]);
}

/// 选中条目起播（外壳 `video::play::play_path` 值进值出；失败重建屏后报错显示）。
fn play_index(mut screen: Screen, path: &str, idx: usize) -> Screen {
    log::info!("ptui: play {} path={}", idx, path);
    screen.draw_str(&format!("播放 {}", path));
    match crate::video::play::play_path(screen, path) {
        Ok(s) => {
            log::info!("ptui: back from play {}", idx);
            s
        }
        Err(e) => {
            log::warn!("ptui: play failed: {:?}", e);
            match Screen::reacquire() {
                Ok(mut s) => {
                    show_err(&mut s, &format!("播放错误: {:?}", e));
                    s
                }
                Err(e2) => {
                    log::warn!("ptui: screen lost: {:?}", e2);
                    loop {
                        stall(Duration::from_secs(60));
                    }
                }
            }
        }
    }
}
