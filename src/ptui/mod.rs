//! TUI 外壳（P3 九宫 + P4 路由）：中文页栈，待机/弹窗按 `p` 进入，`q` 逐层返回。
//!
//! - 入口 `run` 只做一处调用：`pages::home::run(screen)`（九宫首页 +
//!   播放/加速/渲染/调度/解码/显示/测试/壁纸八子页嵌套，`q` 逐层回到此）；
//!   测试页透传 `menu::run_test_page`，播片经 `video::play::play_path` 单路径起播。
//! - 按键约定：数字进页/选片、`+`/`-` 拨档、`q` 逐层返回（子页→首页→待机）。
//!   中文标签直写（31 字表内，见各页；字形由字体 agent 接管）。
//! - 屏显走 `Screen::draw_str`，同步补 `log::` 行：进入打 `ptui: open`，
//!   切页打 `ptui: page=<name>`（各页 `state::goto` 双写，矩阵卡点用），
//!   调档打 `ptui: set <key>=<value>`。
//!
//! GOP 纪律：全程 Shared 借 `Screen`（值进值出各页）；独占 demo 由 `menu` 内部
//! drop/重建；`play_path` 失败（屏已随所有权释放）时播放页 `Screen::reacquire()` 接管。

use crate::graphics::Screen;

/// TUI 入口（值进值出 `Screen`，供待机/弹窗页栈流转；`q` 到底返回调用方）。
pub fn run(screen: Screen) -> Screen {
    log::warn!("ptui: open");
    let screen = pages::home::run(screen);
    log::warn!("ptui: back to idle");
    screen
}

// ---------- P3 TUI 页面模块（TUI agent 新建） ----------
pub mod media;
pub mod pages;
pub mod state;
