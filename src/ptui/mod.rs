//! TUI 外壳（P3 九宫 + P4 路由 + 面板化/悬浮跳转）：中文页栈，待机/弹窗按 `p` 进入，
//! `q`/Esc 逐层返回，Tab 任意页呼出悬浮跳转菜单（`jump`）。
//!
//! - 入口 `run`/`run_at` 只调 `pages::home::run(screen, start)`（首页 + 八子页分发；
//!   浮层跳转经子页返回 `Flow::Jump` 回到首页统一分发，页栈不加深）；
//!   测试页透传 `menu::run_test_page`，播片经 `video::play::play_path` 单路径起播。
//! - 绘制走 `ui`（标题栏 + 边框面板 + 高亮行 + 底栏，整页重画，文本统一
//!   `Screen::draw_text_at`/`draw_str` 同源字体）。
//! - 按键约定：↑↓ 选行、←→ 拨档、Enter 确认；旧数字进页/字母拨档/`+`/`-` 全保留。
//! - 串口：进入打 `ptui: open`，切页打 `ptui: page=<name>`（`state::goto`），
//!   调档打 `ptui: set <key>=<value>`，浮层打 `jump: open/go/close`。
//!
//! GOP 纪律：全程 Shared 借 `Screen`（值进值出各页）；独占 demo 由 `menu` 内部
//! drop/重建；`play_path` 失败（屏已随所有权释放）时播放页 `Screen::reacquire()` 接管。

use crate::graphics::Screen;

/// TUI 退出去向（交给 `idle`）：回壁纸待机，或经浮层去启动选项弹窗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Idle,
    Boot,
}

/// TUI 入口（首页起）。
pub fn run(screen: Screen) -> (Screen, Exit) {
    run_at(screen, state::Next::Home)
}

/// TUI 入口（指定起始页；启动弹窗浮层直跳子页用，子页 `q` 回首页）。
pub fn run_at(screen: Screen, start: state::Next) -> (Screen, Exit) {
    log::warn!("ptui: open");
    // 全量字表自检（矩阵断言 25909，防回退成子集）。
    log::info!("ptui: font cjk glyphs={}", crate::font::cjk::coverage());
    let (screen, exit) = pages::home::run(screen, start);
    match exit {
        Exit::Idle => log::warn!("ptui: back to idle"),
        Exit::Boot => log::warn!("ptui: exit to boot"),
    }
    (screen, exit)
}

// ---------- P3 TUI 页面模块 ----------
pub mod jump;
pub mod media;
pub mod pages;
pub mod state;
pub mod ui;
