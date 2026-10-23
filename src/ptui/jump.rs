//! 悬浮跳转菜单（Tab 呼出的浮层）：TUI 任一页 / 测试页 / 启动选项弹窗内都可呼出，
//! 一键跳到各页面或系统动作（用户拍板：Tab 热键、TUI 全部页 + 启动弹窗）。
//!
//! - 浮层画在当前页之上（不清屏），居中边框框体；↑↓ 选、Enter 去、
//!   直达键（`0`-`8` 页面 / `b` 启动选项 / `w` 壁纸待机 / `f` 固件设置 / `r` 重启 /
//!   `s` 关机）一键去；Tab / Esc / `q` 关闭（页面随即整页重画）。
//! - 去处分三类：TUI 页面（`Target::Page`，由 `pages::home` 分发）、
//!   离开 TUI（`Boot` 回启动弹窗 / `Idle` 回壁纸待机，由 `idle` 接）、
//!   系统动作（固件设置/重启/关机，浮层内当场执行，永不返回；固件设置失败报错后关浮层）。
//! - GOP 纪律：全程借调用方的 `&mut Screen`（Shared），不 drop 不重建。
//! - 串口标记（矩阵卡点）：`jump: open from=<name>` / `jump: go <name>` / `jump: close`。

use alloc::format;
use core::time::Duration;

use uefi::boot::stall;
use uefi::cstr16;

use crate::diag::common::{KEY_DOWN, KEY_ESC, KEY_UP, pause_return, poll_key};
use crate::graphics::Screen;

use super::state::{Next, page_name};
use super::ui;

/// 浮层去处。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Page(Next),
    /// 启动选项弹窗。
    Boot,
    /// 壁纸待机。
    Idle,
    Firmware,
    Reboot,
    Shutdown,
}

/// 子页退出语义：`Back` 回上级；`Jump` 由上级分发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Back,
    Jump(Target),
}

/// 去处串口名（`jump: go <name>`）。
pub fn target_name(t: Target) -> &'static str {
    match t {
        Target::Page(n) => page_name(n),
        Target::Boot => "boot",
        Target::Idle => "idle",
        Target::Firmware => "firmware",
        Target::Reboot => "reboot",
        Target::Shutdown => "shutdown",
    }
}

/// 浮层条目（直达键, 中文名, 去处）。
const ITEMS: [(char, &str, Target); 14] = [
    ('0', "首页", Target::Page(Next::Home)),
    ('1', "播放", Target::Page(Next::Play)),
    ('2', "加速 accel", Target::Page(Next::Accel)),
    ('3', "渲染 render", Target::Page(Next::Render)),
    ('4', "调度 schedule", Target::Page(Next::Schedule)),
    ('5', "解码 decode", Target::Page(Next::Decode)),
    ('6', "显示 display", Target::Page(Next::Display)),
    ('7', "测试 / 诊断", Target::Page(Next::Tests)),
    ('8', "壁纸设置", Target::Page(Next::Wallpaper)),
    ('b', "启动选项 / 引导项", Target::Boot),
    ('w', "回壁纸待机", Target::Idle),
    ('f', "固件设置（重启）", Target::Firmware),
    ('r', "重启", Target::Reboot),
    ('s', "关机", Target::Shutdown),
];

const BOX_W: usize = 300;
const ITEM_H: usize = 22;

/// 呼出浮层；返回选中的页面/离开去处（系统动作当场执行不返回；关闭回 `None`）。
pub fn overlay(screen: &mut Screen, cur: Target) -> Option<Target> {
    log::info!("jump: open from={}", target_name(cur));
    let mut sel = ITEMS.iter().position(|it| it.2 == cur).unwrap_or(0);
    loop {
        draw(screen, cur, sel);
        let (key, _raw) = loop {
            if let Some(k) = poll_key() {
                break k;
            }
            stall(Duration::from_millis(20));
        };
        let pick = match key {
            '\t' | 'q' | KEY_ESC => {
                log::info!("jump: close");
                return None;
            }
            KEY_UP => {
                sel = ui::step_sel(sel, ITEMS.len(), true);
                None
            }
            KEY_DOWN => {
                sel = ui::step_sel(sel, ITEMS.len(), false);
                None
            }
            '\r' | '\n' => Some(ITEMS[sel].2),
            c => ITEMS.iter().find(|it| it.0 == c).map(|it| it.2),
        };
        if let Some(t) = pick {
            log::info!("jump: go {}", target_name(t));
            match t {
                Target::Firmware => {
                    boot_to_firmware(screen);
                    log::info!("jump: close");
                    return None;
                }
                Target::Reboot => uefi::runtime::reset(
                    uefi::runtime::ResetType::COLD,
                    uefi::Status::SUCCESS,
                    None,
                ),
                Target::Shutdown => uefi::runtime::reset(
                    uefi::runtime::ResetType::SHUTDOWN,
                    uefi::Status::SUCCESS,
                    None,
                ),
                _ => return Some(t),
            }
        }
    }
}

/// 浮层框体（居中，阴影 + 琥珀边框；当前位置标 `●`，高亮行蓝底）。
fn draw(screen: &mut Screen, cur: Target, sel: usize) {
    let (sw, sh) = screen.size();
    let h = 36 + ITEMS.len() * ITEM_H + 30;
    let x = sw.saturating_sub(BOX_W) / 2;
    let y = sh.saturating_sub(h) / 2;
    screen.fill_rect(x + 6, y + 6, BOX_W, h, ui::BG);
    screen.fill_rect(x, y, BOX_W, h, ui::PANEL);
    ui::frame(screen, x, y, BOX_W, h, 2, ui::VALUE);
    screen.fill_rect(x + 2, y + 2, BOX_W - 4, 28, ui::BAR);
    screen.draw_text_at(x + 14, y + 8, "快速跳转", ui::WHITE, ui::BAR, BOX_W);
    for (i, (k, name, t)) in ITEMS.iter().enumerate() {
        let ry = y + 36 + i * ITEM_H;
        let on = i == sel;
        let bg = if on { ui::HL } else { ui::PANEL };
        if on {
            screen.fill_rect(x + 4, ry, BOX_W - 8, ITEM_H, ui::HL);
        }
        let fg = if on { ui::WHITE } else { ui::TEXT };
        let kc = if on { ui::WHITE } else { ui::DIM };
        screen.draw_text_at(x + 14, ry + 3, &format!("[{}]", k), kc, bg, 40);
        screen.draw_text_at(x + 54, ry + 3, name, fg, bg, BOX_W - 90);
        if *t == cur {
            screen.draw_text_at(x + BOX_W - 30, ry + 3, "●", ui::OK, bg, 16);
        }
    }
    let hy = y + h - 26;
    screen.draw_text_at(
        x + 14,
        hy,
        "↑↓ Enter 前往  Tab/Esc 关闭",
        ui::DIM,
        ui::PANEL,
        BOX_W - 20,
    );
}

/// 重启进固件设置（BootToFwUi）：置 `OsIndications` bit0 后冷重启。
/// 固件不支持（只读/无变量服务）就报错留下，不 halt。
pub fn boot_to_firmware(screen: &mut Screen) {
    use uefi::runtime::{VariableAttributes, VariableVendor, set_variable};
    log::warn!("popup: firmware setup, resetting...");
    let name = cstr16!("OsIndications");
    // EFI_OS_INDICATIONS_BOOT_TO_FW_UI（bit0），小端 u64。
    let data = 1u64.to_le_bytes();
    let attrs = VariableAttributes::NON_VOLATILE
        | VariableAttributes::BOOTSERVICE_ACCESS
        | VariableAttributes::RUNTIME_ACCESS;
    match set_variable(name, &VariableVendor::GLOBAL_VARIABLE, attrs, &data) {
        Ok(()) => uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None),
        Err(e) => {
            let _ = screen.clear();
            screen.reset_cursor();
            screen.draw_str(&format!("firmware setup unsupported: {:?}", e.status()));
            log::warn!("popup: firmware setup failed: {:?}", e.status());
            pause_return(screen);
        }
    }
}
