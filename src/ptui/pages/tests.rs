//! ptui 测试页：诊断项透传（只调现有 `menu::run_test_page`，不搬诊断项）。
//!
//! - `run_test_page` 内部逐项调 `diag::registry::ITEMS`（播片入口已剔除，
//!   由播放页经 `play_path` 接管）；独占 demo 的 drop/重建、`r` 重启、`q` 返回
//!   全在里面，本页只负责进出打 `ptui: page=tests` marker。
//! - 进页记 `ptui: page=tests`，返回记 `ptui: page=home`（矩阵可断言往返）。

use crate::graphics::Screen;

use super::super::state;

/// 测试页（值进值出 `Screen`；`q` 回首页调用方，`r` 由内页重启）。
pub fn run(screen: Screen) -> Screen {
    log::info!("ptui: page={}", state::page_name(state::Next::Tests));
    let screen = crate::menu::run_test_page(screen);
    log::info!("ptui: page={}", state::page_name(state::Next::Home));
    screen
}
