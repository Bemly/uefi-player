//! ptui 播放页：媒体扫描列表（选中即播）+ 播放档调档。
//!
//! - 列表来自 `media::scan()`（`config::media_dir` 单层目录，`""`=根）；
//!   面板前三行是 fps / feed / 重扫，其下是媒体列表（最多 `VISIBLE` 行可见，
//!   光标越界自动滚动）。↑↓ 选、Enter 播放（停在设置行即拨档/重扫）、←→ 拨档。
//! - 旧快捷键保留：数字键 `1`-`9`/`0` 直播第 1-10 项，`r` 重扫，
//!   `f` 轮换 `play_fps`（auto/max/15/30/60/120），`e` 回绕 `play_feed`
//!   （clip/staged/stream/zero_copy/single）；调档记 `ptui: set play.fps=<v>` /
//!   `ptui: set play.feed=<v>`（只改 runtime 内存）。
//! - 起播一律走 `video::play::play_path`（值进值出 `Screen`；`Err` 时屏已释放，
//!   重建后报错显示）。
//! - 进页记 `ptui: page=play`；起播记 `ptui: play <i> path=<p>`。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use uefi::boot::stall;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::media;
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};
use super::home::show_err;

/// 设置行数（fps / feed / 重扫），媒体行从此下标起。
const HEAD: usize = 3;
/// 媒体列表可见行数（超出滚动）。
const VISIBLE: usize = 16;
/// 数字直达键（第 1-10 项）。
const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

/// 播放页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Play));
    flush_keys();
    let mut list: Vec<String> = Vec::new();
    let mut err: Option<String> = None;
    rescan(&mut list, &mut err);
    let mut sel: usize = if list.is_empty() { 0 } else { HEAD };
    loop {
        draw(&mut screen, &list, err.as_deref(), sel);
        let act = ui::read(&mut screen, Target::Page(Next::Play));
        let n = HEAD + list.len();
        let pick = match act {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up => {
                sel = ui::step_sel(sel, n, true);
                None
            }
            Act::Down => {
                sel = ui::step_sel(sel, n, false);
                None
            }
            Act::Left if sel < 2 => {
                adjust(&mut screen, sel, -1);
                None
            }
            Act::Right if sel < 2 => {
                adjust(&mut screen, sel, 1);
                None
            }
            Act::Enter if sel < 2 => {
                adjust(&mut screen, sel, 1);
                None
            }
            Act::Enter if sel == 2 => {
                rescan(&mut list, &mut err);
                sel = sel.min(HEAD + list.len().saturating_sub(1));
                None
            }
            Act::Enter => Some(sel - HEAD),
            Act::Key('r') => {
                rescan(&mut list, &mut err);
                sel = sel.min(HEAD + list.len().saturating_sub(1));
                None
            }
            Act::Key('f') => {
                adjust(&mut screen, 0, 1);
                None
            }
            Act::Key('e') => {
                adjust(&mut screen, 1, 1);
                None
            }
            Act::Key(d) if d.is_ascii_digit() => {
                let i = if d == '0' {
                    9
                } else {
                    (d as u8 - b'1') as usize
                };
                Some(i)
            }
            _ => None,
        };
        if let Some(i) = pick.filter(|&i| i < list.len()) {
            sel = HEAD + i;
            screen = play_index(screen, &list[i], i);
            state::goto(&mut screen, state::page_name(Next::Play));
            flush_keys();
        }
    }
}

/// 重扫媒体目录（失败记错误行，列表清空）。
fn rescan(list: &mut Vec<String>, err: &mut Option<String>) {
    match media::scan() {
        Ok(v) => {
            *list = v;
            *err = None;
        }
        Err(e) => {
            list.clear();
            *err = Some(format!("扫描错误: {:?}", e));
        }
    }
}

/// 播放页面板（设置三行 + 媒体列表窗口）。
fn draw(screen: &mut Screen, list: &[String], err: Option<&str>, sel: usize) {
    let rt = crate::config::runtime();
    let mut rows: Vec<Row> = Vec::with_capacity(HEAD + VISIBLE);
    rows.push(Row::new(
        "f",
        "play.fps",
        format!("{}  (auto/max/15/30/60/120)", rt.play_fps),
    ));
    rows.push(Row::new(
        "e",
        "play.feed",
        format!(
            "{}  (clip/staged/stream/zero_copy/single)",
            state::FEED_STRS[rt.play_feed.as_u8() as usize]
        ),
    ));
    rows.push(Row::new("r", "重新扫描", format!("{} 个文件", list.len())));
    // 媒体窗口：光标在列表内时让它保持可见。
    let cur = sel.saturating_sub(HEAD);
    let start = if sel >= HEAD && cur >= VISIBLE {
        cur + 1 - VISIBLE
    } else {
        0
    };
    for (i, name) in list.iter().enumerate().skip(start).take(VISIBLE) {
        let key = DIGITS.get(i).copied().unwrap_or("");
        rows.push(Row::new(key, name, String::new()));
    }
    let shown_sel = if sel >= HEAD { sel - start } else { sel };
    let mut notes: Vec<String> = Vec::new();
    if let Some(e) = err {
        notes.push(String::from(e));
    } else if list.is_empty() {
        notes.push(String::from("媒体目录为空：放好片源后按 r 重扫"));
    } else if list.len() > VISIBLE {
        notes.push(format!(
            "显示 {}-{} / 共 {} 项",
            start + 1,
            (start + VISIBLE).min(list.len()),
            list.len()
        ));
    }
    ui::draw(
        screen,
        &View {
            title: "播放",
            rows: &rows,
            sel: Some(shown_sel),
            notes: &notes,
            hint: "↑↓ 选择  Enter 播放  0-9 直播  f/e 拨档  r 重扫  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

/// 拨 fps（0）或 feed（1）（只改 runtime 内存）。
fn adjust(screen: &mut Screen, which: usize, delta: i64) {
    if which == 0 {
        let cur = crate::config::runtime().play_fps.clone();
        let idx = state::FPS_SEQ.iter().position(|&s| s == cur).unwrap_or(0);
        let n = state::FPS_SEQ.len() as i64;
        let next = state::FPS_SEQ[(idx as i64 + delta).rem_euclid(n) as usize];
        crate::config::runtime_mut().play_fps = String::from(next);
        state::set(screen, "play.fps", next);
    } else {
        let next = state::step_feed(crate::config::runtime().play_feed, delta as i8);
        crate::config::runtime_mut().play_feed = next;
        state::set(screen, "play.feed", state::FEED_STRS[next.as_u8() as usize]);
    }
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
