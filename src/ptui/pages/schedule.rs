//! ptui 调度页：后端/多核/分工/角色/核预算（只改 runtime 内存）。
//!
//! - 面板五行：backend / workers / dist / roles / cores；↑↓ 选行、←→ 拨、Enter 同 →。
//! - 旧快捷键保留：`b` 回绕 backend（manual/nano/dynamic），`w` workers
//!   （auto/bsp_only/all），`d` dist（auto/static/tile/dynamic），`r` roles
//!   （auto/combined/split），`+`/`-` 调 cores（0-64，0=不限）。
//! - 每次调档记 `ptui: set schedule.*=<v>`；进页记 `ptui: page=schedule`。

use alloc::format;
use alloc::string::String;

use crate::diag::common::flush_keys;
use crate::graphics::Screen;

use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};

const N: usize = 5;

/// 调度页主循环（值进值出 `Screen`；`q` 返回首页调用方）。
pub fn run(mut screen: Screen) -> (Screen, Flow) {
    state::goto(&mut screen, state::page_name(Next::Schedule));
    flush_keys();
    let mut sel: usize = 0;
    loop {
        draw(&mut screen, sel);
        match ui::read(&mut screen, Target::Page(Next::Schedule)) {
            Act::Back => return (screen, Flow::Back),
            Act::Jump(t) => return (screen, Flow::Jump(t)),
            Act::Up => sel = ui::step_sel(sel, N, true),
            Act::Down => sel = ui::step_sel(sel, N, false),
            Act::Right | Act::Enter => adjust(&mut screen, sel, 1),
            Act::Left => adjust(&mut screen, sel, -1),
            Act::Key('b') => adjust(&mut screen, 0, 1),
            Act::Key('w') => adjust(&mut screen, 1, 1),
            Act::Key('d') => adjust(&mut screen, 2, 1),
            Act::Key('r') => adjust(&mut screen, 3, 1),
            Act::Key('+' | '=') => adjust(&mut screen, 4, 1),
            Act::Key('-' | '_') => adjust(&mut screen, 4, -1),
            _ => {}
        }
    }
}

/// 调度页面板。
fn draw(screen: &mut Screen, sel: usize) {
    let rt = crate::config::runtime();
    let s = |v: &str, all: &str| format!("{}  ({})", v, all);
    let rows = [
        Row::new(
            "b",
            "schedule.backend",
            s(
                state::BACKEND_STRS[rt.schedule_backend.as_u8() as usize],
                "manual/nano/dynamic",
            ),
        ),
        Row::new(
            "w",
            "schedule.workers",
            s(
                state::WORKERS_STRS[rt.schedule_workers.as_u8() as usize],
                "auto/bsp_only/all",
            ),
        ),
        Row::new(
            "d",
            "schedule.dist",
            s(
                state::DIST_STRS[rt.schedule_dist.as_u8() as usize],
                "auto/static/tile/dynamic",
            ),
        ),
        Row::new(
            "r",
            "schedule.roles",
            s(
                state::ROLES_STRS[rt.schedule_roles.as_u8() as usize],
                "auto/combined/split",
            ),
        ),
        Row::new(
            "+-",
            "schedule.cores",
            format!("{}  (0 = 不限)", rt.schedule_cores),
        ),
    ];
    let notes = [String::from("只改内存，本次开机内生效（不落盘）")];
    ui::draw(
        screen,
        &View {
            title: "调度 schedule",
            rows: &rows,
            sel: Some(sel),
            notes: &notes,
            hint: "↑↓ 选择  ←→ 调整  b/w/d/r 开关  +- 核数  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

/// 调一行（开关回绕；cores ±1 钳 0-64，与 `runtime::apply_cfg` 一致；只改 runtime 内存）。
fn adjust(screen: &mut Screen, sel: usize, delta: i8) {
    let rt = crate::config::runtime_mut();
    match sel {
        0 => {
            let next = state::step_backend(rt.schedule_backend, delta);
            rt.schedule_backend = next;
            state::set(
                screen,
                "schedule.backend",
                state::BACKEND_STRS[next.as_u8() as usize],
            );
        }
        1 => {
            let next = state::step_workers(rt.schedule_workers, delta);
            rt.schedule_workers = next;
            state::set(
                screen,
                "schedule.workers",
                state::WORKERS_STRS[next.as_u8() as usize],
            );
        }
        2 => {
            let next = state::step_dist(rt.schedule_dist, delta);
            rt.schedule_dist = next;
            state::set(
                screen,
                "schedule.dist",
                state::DIST_STRS[next.as_u8() as usize],
            );
        }
        3 => {
            let next = state::step_roles(rt.schedule_roles, delta);
            rt.schedule_roles = next;
            state::set(
                screen,
                "schedule.roles",
                state::ROLES_STRS[next.as_u8() as usize],
            );
        }
        _ => {
            let v = (rt.schedule_cores as i64 + delta as i64).clamp(0, 64) as u32;
            rt.schedule_cores = v;
            state::set(screen, "schedule.cores", &format!("{}", v));
        }
    }
}
