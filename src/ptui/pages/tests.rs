//! ptui 测试页：诊断项透传（只调现有 `menu::run_test_page`，不搬诊断项）。
//!
//! - `run_test_page` 内部逐项调 `diag::registry::ITEMS`（播片入口已剔除，
//!   由播放页经 `play_path` 接管）；独占 demo 的 drop/重建、`r` 重启、`q` 返回、
//!   Tab 浮层全在里面，本页只负责进出打 `ptui: page=tests` marker。
//! - 进页记 `ptui: page=tests`（返回后由首页重记 `ptui: page=home`）。

use crate::graphics::Screen;

use super::super::jump::Flow;
use super::super::state;

/// 测试页（值进值出 `Screen`；`q` 回首页调用方，Tab 浮层跳转回 `Flow::Jump`）。
pub fn run(screen: Screen) -> (Screen, Flow) {
    log::info!("ptui: page={}", state::page_name(state::Next::Tests));
    crate::menu::run_test_page(screen)
}
