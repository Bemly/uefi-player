//! ptui 调度页：后端/多核/分工/核预算/角色（只改 runtime 内存）。
//!
//! - `b` 回绕 `schedule.backend`（manual/nano/dynamic），`w` 回绕 workers
//!   （auto/bsp_only/all），`d` 回绕 dist（auto/static/tile/dynamic），
//!   `r` 回绕 roles（auto/combined/split），`+`/`-` 调 cores（0-64，0=不限）。
//! - 每次调档双写 `ptui: set schedule.*=<v>`；进页记 `ptui: page=schedule`。

use alloc::format;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::state;
use super::home::wait_key;

/// 调度页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> Screen {
    state::goto(&mut screen, state::page_name(state::Next::Schedule));
    flush_keys();
    loop {
        draw(&mut screen);
        let Some((key, _raw)) = wait_key() else {
            continue;
        };
        match key {
            'q' => return screen,
            'b' => {
                let next = state::step_backend(crate::config::runtime().schedule_backend, 1);
                crate::config::runtime_mut().schedule_backend = next;
                state::set(
                    &mut screen,
                    "schedule.backend",
                    state::BACKEND_STRS[next.as_u8() as usize],
                );
            }
            'w' => {
                let next = state::step_workers(crate::config::runtime().schedule_workers, 1);
                crate::config::runtime_mut().schedule_workers = next;
                state::set(
                    &mut screen,
                    "schedule.workers",
                    state::WORKERS_STRS[next.as_u8() as usize],
                );
            }
            'd' => {
                let next = state::step_dist(crate::config::runtime().schedule_dist, 1);
                crate::config::runtime_mut().schedule_dist = next;
                state::set(
                    &mut screen,
                    "schedule.dist",
                    state::DIST_STRS[next.as_u8() as usize],
                );
            }
            'r' => {
                let next = state::step_roles(crate::config::runtime().schedule_roles, 1);
                crate::config::runtime_mut().schedule_roles = next;
                state::set(
                    &mut screen,
                    "schedule.roles",
                    state::ROLES_STRS[next.as_u8() as usize],
                );
            }
            '+' | '=' => cores(&mut screen, 1),
            '-' | '_' => cores(&mut screen, -1),
            _ => {}
        }
    }
}

/// 调度页屏显（五开关行 + 按键约定行）。
fn draw(screen: &mut Screen) {
    let rt = crate::config::runtime();
    screen.draw_str("== schedule ==");
    screen.draw_str(&format!(
        "[b] backend={} (manual/nano/dynamic)",
        state::BACKEND_STRS[rt.schedule_backend.as_u8() as usize]
    ));
    screen.draw_str(&format!(
        "[w] workers={} (auto/bsp_only/all)",
        state::WORKERS_STRS[rt.schedule_workers.as_u8() as usize]
    ));
    screen.draw_str(&format!(
        "[d] dist={} (auto/static/tile/dynamic)",
        state::DIST_STRS[rt.schedule_dist.as_u8() as usize]
    ));
    screen.draw_str(&format!(
        "[r] roles={} (auto/combined/split)",
        state::ROLES_STRS[rt.schedule_roles.as_u8() as usize]
    ));
    screen.draw_str(&format!("[+/-] cores={} (0: no limit)", rt.schedule_cores));
    screen.draw_str("[b/w/d/r] [+/-] [q] 返回");
}

/// 核预算 ±1（0-64 钳制，与 `runtime::apply_cfg` 一致；只改 runtime 内存）。
fn cores(screen: &mut Screen, delta: i64) {
    let rt = crate::config::runtime_mut();
    let v = (rt.schedule_cores as i64 + delta).clamp(0, 64) as u32;
    rt.schedule_cores = v;
    state::set(screen, "schedule.cores", &format!("{}", v));
}
