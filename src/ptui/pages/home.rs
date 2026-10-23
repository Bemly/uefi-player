//! ptui 首页：面板导航（播放/加速/渲染/调度/解码/显示/测试/壁纸 + 启动选项 + 返回）
//! + 子页分发中枢。
//!
//! - ↑↓ 选、Enter 进；数字键 `1`-`8` 直进子页（旧口径保留），`b` 去启动选项，
//!   `q`/Esc 回外壳调用方。Tab 浮层的去处也在这里统一分发：子页返回
//!   `Flow::Jump(Page(n))` 即由本页接着进 `n`（页栈不加深），`Boot`/`Idle` 退出 TUI。
//! - 进页记 `ptui: page=home`（`state::goto`）。

use core::time::Duration;

use alloc::string::String;
use uefi::boot::stall;

use crate::diag::common::{flush_keys, poll_key};
use crate::graphics::Screen;

use super::super::Exit;
use super::super::jump::{Flow, Target};
use super::super::state::{self, Next};
use super::super::ui::{self, Act, Row, View};
use super::{accel, decode, display, play, render, schedule, tests, wallpaper};

/// 首页条目（快捷键, 标题, 说明, 去处）。
const ITEMS: [(&str, &str, &str, Target); 10] = [
    (
        "1",
        "播放",
        "扫描媒体目录，选片即播",
        Target::Page(Next::Play),
    ),
    (
        "2",
        "加速 accel",
        "指令集档位（31 档）",
        Target::Page(Next::Accel),
    ),
    (
        "3",
        "渲染 render",
        "路径 / vsync / 引擎 / 微调",
        Target::Page(Next::Render),
    ),
    (
        "4",
        "调度 schedule",
        "后端 / 多核 / 分工 / 角色",
        Target::Page(Next::Schedule),
    ),
    (
        "5",
        "解码 decode",
        "八格式线程 / 缓冲 / 供片",
        Target::Page(Next::Decode),
    ),
    (
        "6",
        "显示 display",
        "适配 / 锚点 / 黑边",
        Target::Page(Next::Display),
    ),
    (
        "7",
        "测试 / 诊断",
        "硬件探测与基准",
        Target::Page(Next::Tests),
    ),
    (
        "8",
        "壁纸设置",
        "开关 / 帧率 / 预载，可落盘",
        Target::Page(Next::Wallpaper),
    ),
    (
        "b",
        "启动选项",
        "引导项 / 固件设置 / 重启关机",
        Target::Boot,
    ),
    ("q", "返回", "回壁纸待机", Target::Idle),
];

/// 首页主循环（值进值出 `Screen`；`start` 非首页时先进该子页）。
pub fn run(mut screen: Screen, start: Next) -> (Screen, Exit) {
    let mut pending = if start == Next::Home {
        None
    } else {
        Some(start)
    };
    let mut sel = 0usize;
    loop {
        // 分发（直进/浮层跳转链）：子页回 `Back` 落回首页，回 `Jump` 继续分发。
        while let Some(n) = pending.take() {
            if let Some(i) = ITEMS.iter().position(|it| it.3 == Target::Page(n)) {
                sel = i;
            }
            let (s, flow) = dispatch(screen, n);
            screen = s;
            match flow {
                Flow::Back | Flow::Jump(Target::Page(Next::Home)) => {}
                Flow::Jump(Target::Page(m)) => pending = Some(m),
                Flow::Jump(Target::Boot) => return (screen, Exit::Boot),
                Flow::Jump(_) => return (screen, Exit::Idle),
            }
        }
        state::goto(&mut screen, state::page_name(Next::Home));
        flush_keys();
        loop {
            draw(&mut screen, sel);
            let act = ui::read(&mut screen, Target::Page(Next::Home));
            let pick = match act {
                Act::Back => Some(Target::Idle),
                Act::Jump(t) => Some(t),
                Act::Up => {
                    sel = ui::step_sel(sel, ITEMS.len(), true);
                    None
                }
                Act::Down => {
                    sel = ui::step_sel(sel, ITEMS.len(), false);
                    None
                }
                Act::Enter | Act::Right => Some(ITEMS[sel].3),
                Act::Key(c) => {
                    let hit = ITEMS
                        .iter()
                        .find(|it| it.0.len() == 1 && it.0.starts_with(c) && c != 'q');
                    if hit.is_none() {
                        ui::set_status(String::from("未知按键：↑↓ Enter / 1-8 / b / q"));
                    }
                    hit.map(|it| it.3)
                }
                Act::Left | Act::Redraw => None,
            };
            match pick {
                None => {}
                Some(Target::Page(Next::Home)) => {}
                Some(Target::Page(n)) => {
                    pending = Some(n);
                    break;
                }
                Some(Target::Boot) => return (screen, Exit::Boot),
                Some(_) => {
                    log::info!("ptui: back to caller");
                    return (screen, Exit::Idle);
                }
            }
        }
    }
}

/// 进子页（各页值进值出 `Screen`，回 `Flow`）。
fn dispatch(screen: Screen, n: Next) -> (Screen, Flow) {
    match n {
        Next::Play => play::run(screen),
        Next::Accel => accel::run(screen),
        Next::Render => render::run(screen),
        Next::Schedule => schedule::run(screen),
        Next::Decode => decode::run(screen),
        Next::Display => display::run(screen),
        Next::Tests => tests::run(screen),
        Next::Wallpaper => wallpaper::run(screen),
        Next::Home => (screen, Flow::Back),
    }
}

/// 首页面板。
fn draw(screen: &mut Screen, sel: usize) {
    let rows: [Row; 10] = core::array::from_fn(|i| {
        let (k, name, desc, _) = ITEMS[i];
        Row::new(k, name, String::from(desc))
    });
    ui::draw(
        screen,
        &View {
            title: "播放器菜单",
            rows: &rows,
            sel: Some(sel),
            notes: &[],
            hint: "↑↓ 选择  Enter 进入  1-8 直达  Tab 跳转  q 返回",
            compact: false,
        },
    );
}

/// 子页共用的阻塞等键（屏显-only 的确认/报错页用）。
pub fn wait_any(screen: &mut Screen) {
    screen.draw_str("按键继续");
    flush_keys();
    loop {
        if poll_key().is_some() {
            return;
        }
        stall(Duration::from_millis(50));
    }
}

/// 子页共用的屏显报错（串口 warn + 屏显 + 等键）。
pub fn show_err(screen: &mut Screen, msg: &str) {
    log::warn!("{}", msg);
    screen.draw_str(msg);
    wait_any(screen);
}
