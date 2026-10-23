//! TUI 绘制层：标题栏 + 边框面板（高亮行）+ 底部按键提示/状态栏 + 统一按键映射。
//!
//! - 轮子：固件侧 `no_std` + GOP 直写，`ratatui`（需 `std` 终端后端）/`embedded-graphics`
//!   （已有依赖，但 `DrawTarget` 逐像素，且主屏文本口径是 `Screen::draw_str` 同源字体）
//!   都不合本仓“文本统一走 `Screen` 字体”的口径，故面板只用 `Screen::fill_rect` +
//!   `Screen::draw_text_at` 两个原语自绘（见 `graphics/text.rs`）。
//! - 布局：整页重画（清屏 → 标题栏 → 面板 → 备注行 → 底栏），一段文本一次 Blt，
//!   每次按键重画一遍即可，无需脏区。
//! - 按键（`read`/`map`）：↑↓ 选行、←→ 拨档、Enter 确认（拨 +1 / 进入）、
//!   `q`/Esc 返回、Tab 呼出悬浮跳转菜单（`jump::overlay`）；
//!   其余字符原样交给各页（旧数字/字母快捷键全保留，矩阵与老用户零感知）。
//! - 状态：`state::set` 调档时记一条状态（底栏右侧显示），串口仍由 `state::set` 打。

use alloc::string::String;
use core::time::Duration;

use uefi::boot::stall;
use uefi::proto::console::gop::BltPixel;

use crate::diag::common::{KEY_DOWN, KEY_END, KEY_ESC, KEY_HOME, KEY_LEFT, KEY_PGDN, KEY_PGUP};
use crate::diag::common::{KEY_RIGHT, KEY_UP, poll_key};
use crate::graphics::Screen;

use super::jump::{self, Target};

// ---------- 主题（深蓝底 + 蓝色强调 + 琥珀值） ----------
pub const BG: BltPixel = BltPixel::new(14, 18, 30);
pub const PANEL: BltPixel = BltPixel::new(26, 32, 50);
pub const BORDER: BltPixel = BltPixel::new(72, 96, 150);
pub const BAR: BltPixel = BltPixel::new(36, 86, 168);
pub const TEXT: BltPixel = BltPixel::new(222, 228, 238);
pub const DIM: BltPixel = BltPixel::new(132, 144, 166);
pub const HL: BltPixel = BltPixel::new(58, 122, 214);
pub const WHITE: BltPixel = BltPixel::new(255, 255, 255);
pub const VALUE: BltPixel = BltPixel::new(255, 202, 96);
pub const OK: BltPixel = BltPixel::new(126, 214, 146);

/// 标题栏/底栏高、行高、面板左缘、面板顶。
pub const BAR_H: usize = 28;
pub const ROW_H: usize = 24;
pub const PAD_X: usize = 32;
pub const TOP: usize = BAR_H + 16;
/// 列：快捷键 / 标签 / 值（相对面板左缘）。
const COL_KEY: usize = 14;
const COL_LABEL: usize = 64;
const COL_VALUE: usize = 300;

/// 面板一行：快捷键提示（可空）、标签、值（可空）。
pub struct Row<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub value: String,
}

impl<'a> Row<'a> {
    pub fn new(key: &'a str, label: &'a str, value: String) -> Self {
        Self { key, label, value }
    }
}

/// 一页视图：标题、行、高亮行、行下备注、底栏提示；`compact` 时底栏紧贴内容
/// （行区下方要播壁纸的页用，返回值即动画可用的起始行）。
pub struct View<'a> {
    pub title: &'a str,
    pub rows: &'a [Row<'a>],
    pub sel: Option<usize>,
    pub notes: &'a [String],
    pub hint: &'a str,
    pub compact: bool,
}

/// 状态行（BSP 单线程 UI 写读，口径同 `config::runtime_mut`）。
static mut STATUS: Option<String> = None;

/// 记一条状态（底栏右侧显示，下一次 `clear_status`/进页清掉）。
pub fn set_status(s: String) {
    // 安全：UI 只在 BSP 单线程跑，无并发读写。
    let p = &raw mut STATUS;
    unsafe { *p = Some(s) };
}

pub fn clear_status() {
    // 安全：同上。
    let p = &raw mut STATUS;
    unsafe { *p = None };
}

fn status() -> Option<&'static str> {
    // 安全：同上。
    let p = &raw const STATUS;
    unsafe { (*p).as_deref() }
}

/// 面板宽（屏宽减两侧留白，封顶 1000）。
pub fn panel_w(screen: &Screen) -> usize {
    let (sw, _) = screen.size();
    sw.saturating_sub(PAD_X * 2).min(1000)
}

/// 空心框（`t` px 线宽）。
pub fn frame(screen: &mut Screen, x: usize, y: usize, w: usize, h: usize, t: usize, c: BltPixel) {
    screen.fill_rect(x, y, w, t, c);
    screen.fill_rect(x, y + h.saturating_sub(t), w, t, c);
    screen.fill_rect(x, y, t, h, c);
    screen.fill_rect(x + w.saturating_sub(t), y, t, h, c);
}

/// 整页重画；返回已画内容底边（`compact` 时含底栏）。
pub fn draw(screen: &mut Screen, v: &View) -> usize {
    let (sw, sh) = screen.size();
    screen.reset_cursor();
    screen.fill_rect(0, 0, sw, sh, BG);
    // 标题栏：左侧 应用名 > 页名，右侧 Tab 提示。
    screen.fill_rect(0, 0, sw, BAR_H, BAR);
    let x = screen.draw_text_at(PAD_X, 6, "uefi-player", WHITE, BAR, sw);
    screen.draw_text_at(PAD_X + x, 6, "  >  ", DIM, BAR, sw);
    let x2 = x + crate::font::str_width_px("  >  ");
    screen.draw_text_at(PAD_X + x2, 6, v.title, WHITE, BAR, sw);
    let tab = "Tab 跳转菜单";
    let tw = crate::font::str_width_px(tab);
    screen.draw_text_at(sw.saturating_sub(PAD_X + tw), 6, tab, WHITE, BAR, sw);

    // 面板：边框 + 行（高亮行整条蓝底）。
    let pw = panel_w(screen);
    let ph = v.rows.len().max(1) * ROW_H + 16;
    screen.fill_rect(PAD_X, TOP, pw, ph, PANEL);
    frame(screen, PAD_X, TOP, pw, ph, 2, BORDER);
    for (i, r) in v.rows.iter().enumerate() {
        let y = TOP + 8 + i * ROW_H;
        let on = v.sel == Some(i);
        let bg = if on { HL } else { PANEL };
        if on {
            screen.fill_rect(PAD_X + 4, y, pw - 8, ROW_H, HL);
            screen.draw_text_at(PAD_X + 4, y + 4, "▶", WHITE, bg, 12);
        }
        let ty = y + 4;
        let kc = if on { WHITE } else { DIM };
        screen.draw_text_at(PAD_X + COL_KEY, ty, r.key, kc, bg, COL_LABEL - COL_KEY);
        let lc = if on { WHITE } else { TEXT };
        let lw = if r.value.is_empty() {
            pw - COL_LABEL - 8
        } else {
            COL_VALUE - COL_LABEL - 8
        };
        screen.draw_text_at(PAD_X + COL_LABEL, ty, r.label, lc, bg, lw);
        if !r.value.is_empty() {
            let vc = if on { WHITE } else { VALUE };
            let vw = pw.saturating_sub(COL_VALUE + 8);
            screen.draw_text_at(PAD_X + COL_VALUE, ty, &r.value, vc, bg, vw);
        }
    }
    // 备注行（面板下方，暗色）。
    let mut y = TOP + ph + 8;
    for n in v.notes {
        screen.draw_text_at(PAD_X, y, n, DIM, BG, pw);
        y += ROW_H - 4;
    }
    // 底栏：紧贴内容（compact）或贴屏底。
    let fy = if v.compact {
        y + 4
    } else {
        sh.saturating_sub(BAR_H)
    };
    screen.fill_rect(0, fy, sw, BAR_H, PANEL);
    screen.fill_rect(0, fy, sw, 1, BORDER);
    screen.draw_text_at(PAD_X, fy + 6, v.hint, DIM, PANEL, sw);
    if let Some(s) = status() {
        let w = crate::font::str_width_px(s);
        screen.draw_text_at(sw.saturating_sub(PAD_X + w), fy + 6, s, OK, PANEL, sw);
    }
    fy + BAR_H
}

/// 统一按键语义。
pub enum Act {
    /// `q` / Esc。
    Back,
    /// 浮层选中了去处。
    Jump(Target),
    /// 浮层关闭，页面需重画。
    Redraw,
    Up,
    Down,
    Left,
    Right,
    /// Enter（回车/换行）。
    Enter,
    /// 其余字符（各页旧快捷键）。
    Key(char),
}

/// 原始键 → 语义（Tab 当场开浮层；`cur` 是浮层“当前位置”标记与串口 from 名）。
pub fn map(screen: &mut Screen, cur: Target, key: char) -> Act {
    match key {
        '\t' => match jump::overlay(screen, cur) {
            Some(t) => Act::Jump(t),
            None => Act::Redraw,
        },
        'q' | KEY_ESC => Act::Back,
        KEY_UP | KEY_PGUP | KEY_HOME => Act::Up,
        KEY_DOWN | KEY_PGDN | KEY_END => Act::Down,
        KEY_LEFT => Act::Left,
        KEY_RIGHT => Act::Right,
        '\r' | '\n' => Act::Enter,
        c => Act::Key(c),
    }
}

/// 阻塞读一键并映射（无壁纸动画的页用）。
pub fn read(screen: &mut Screen, cur: Target) -> Act {
    loop {
        if let Some((k, _raw)) = poll_key() {
            return map(screen, cur, k);
        }
        stall(Duration::from_millis(20));
    }
}

/// 选中行上下移动（回绕；`n == 0` 原样）。
pub fn step_sel(sel: usize, n: usize, up: bool) -> usize {
    if n == 0 {
        0
    } else if up {
        (sel + n - 1) % n
    } else {
        (sel + 1) % n
    }
}
