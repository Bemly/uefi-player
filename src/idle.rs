//! 开机空闲层（顶层交互入口）：壁纸待机 + 启动选项弹窗。
//!
//! 流程：`main` → 校准（首轮无记忆必测，即“首轮默认测试”）→ `idle::run`（永不返回）：
//! 壁纸全屏循环（无片源就黑屏 + 提示行）；任意键 → 按 `[wallpaper] popup_freeze`
//! 冻结或继续播行动画 → 弹出启动选项。选条目交权（回来继续待机）；
//! `p` 进纯播放器菜单（`q` 回来）；`f` 重启进固件设置（BootToFwUi）；
//! `s` 关机；`r` 重启；`q` 回壁纸。串口标记见各 `log::` 行（矩阵靠它们同步）。

use alloc::format;
use core::time::Duration;

use uefi::boot::stall;
use uefi::cstr16;

use crate::boot::{self, KEYS, entry_line};
use crate::diag::common::{flush_keys, pause_return, poll_key};
use crate::error::Result;
use crate::graphics::{Screen, calibrate_tps, tsc};
use crate::menu::run_menu;
use crate::video::decoder::Clip;
use crate::wallpaper::{self, WP_H, WP_TOP, WP_W};

/// 顶层循环：待机 → 弹窗 → 动作 → 回待机（交权/播放器返回后亦然；
/// 关机/重启/固件设置永不返回）。
pub fn run(mut screen: Screen) -> ! {
    loop {
        // 每次回待机都重载壁纸（U盘热插拔换片源即生效；便宜）。
        let clip = wallpaper::load().unwrap_or(None);
        let tps = if clip.is_some() { calibrate_tps() } else { 1 };
        idle_wait(&mut screen, &clip, tps);
        screen = match popup(screen, &clip, tps) {
            Ok(s) => s,
            Err(e) => {
                // 重建屏显都失败：halt（理论到不了，交权失败路径在别处已处理）。
                log::warn!("popup: screen lost: {:?}", e);
                loop {
                    stall(Duration::from_secs(60));
                }
            }
        };
    }
}

/// 壁纸待机：全屏循环（`fps` 预算）；无片源/分辨率不对就黑屏 + 提示行。
/// 任意键（消耗掉）即返回，调用方开弹窗。串口 `idle: ready` 供矩阵卡点。
fn idle_wait(screen: &mut Screen, clip: &Option<Clip>, tps: u64) {
    // 先排空陈旧输入再打标记：标记之后到的键才是有效按键（矩阵靠标记卡点）。
    // 弹窗内不再排空——开弹窗的键已被上轮 poll 消耗，后到的都是真输入。
    flush_keys();
    log::info!("idle: ready (press any key)");
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    let live = match clip {
        Some(c) if !c.is_empty() && (w as usize, h as usize) == (WP_W, WP_H) => true,
        _ => {
            let _ = screen.clear();
            screen.reset_cursor();
            screen.draw_str("press any key for boot options...");
            false
        }
    };
    let budget = tps.max(1) / crate::config::wallpaper_fps().max(1) as u64;
    let mut idx = 0usize;
    let mut last = tsc();
    flush_keys();
    loop {
        if poll_key().is_some() {
            return;
        }
        if live {
            let now = tsc();
            if now.saturating_sub(last) >= budget {
                last = now;
                let clip = clip.as_ref().unwrap_or_else(|| unreachable!());
                let _ = screen.upload_frame(WP_W, WP_H, clip.frame(idx));
                idx += 1;
            }
        } else {
            stall(Duration::from_millis(5));
        }
    }
}

/// 启动选项弹窗：条目数字键直选交权；`p/f/s/r` 固定项；`q` 回壁纸；
/// `Enter` 跑默认项。`frozen` 时静态屏，`live` 时行区继续播（选项占文字区）。
fn popup(mut screen: Screen, clip: &Option<Clip>, tps: u64) -> Result<Screen> {
    let list = boot::list()?;
    // 冻结语义：开关关 / 无片源 / 分辨率不对 → 静态（行区无东西可播）。
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    let can_live = !clip.as_ref().map(|c| c.is_empty()).unwrap_or(true)
        && (w as usize, h as usize) == (WP_W, WP_H);
    let frozen = !can_live || crate::config::wallpaper_popup_freeze();
    draw_popup(&mut screen, &list);
    // 弹窗行数（标题 1 + 警告 + 空行 1 + 条目 + 空行 1 + 固定 5 行）：
    // live 动画只画此行之下，避免盖字。
    let text_end = (2 + list.warnings.len() + list.entries.len() + 1 + 5) * 18 + 8;
    let anim_top = WP_TOP.max(text_end);
    log::warn!("popup: open ({})", if frozen { "frozen" } else { "live" });

    let budget = tps.max(1) / crate::config::wallpaper_fps().max(1) as u64;
    let mut idx = 0usize;
    let mut last = tsc();
    // 注意：这里不 flush——开弹窗的键已被待机 poll 消耗，列表枚举期间到达的
    // 都是真输入；flush 会把矩阵紧随其后的同步键冲掉（曾因此全员超时）。
    loop {
        if !frozen {
            let now = tsc();
            if now.saturating_sub(last) >= budget {
                last = now;
                if let Some(c) = clip {
                    let _ = screen.upload_frame_region(WP_W, WP_H, anim_top, c.frame(idx));
                    idx += 1;
                }
            }
        }
        let Some((key, _raw)) = poll_key() else {
            if frozen {
                stall(Duration::from_millis(5));
            }
            continue;
        };
        // 固定项（先判；`q` 永不进条目表）。
        match key {
            'q' => {
                log::warn!("popup: back to idle");
                return Ok(screen);
            }
            'p' => {
                log::warn!("popup: player mode");
                return Ok(run_menu(screen));
            }
            'f' => {
                boot_to_firmware(&mut screen);
                draw_popup(&mut screen, &list);
                continue;
            }
            's' => {
                log::warn!("popup: shutdown");
                uefi::runtime::reset(
                    uefi::runtime::ResetType::SHUTDOWN,
                    uefi::Status::SUCCESS,
                    None,
                );
            }
            'r' => {
                log::warn!("popup: reboot");
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None);
            }
            '\r' | '\n' => {
                if let Some(i) = list.default_index {
                    screen = boot::boot_index(screen, &list, clip, tps, i)?;
                    return Ok(screen);
                }
            }
            _ => {}
        }
        // 条目键（`p/f/s/r` 已被固定项占，`q` 已返回；剩下的才查表）。
        if let Some(pos) = KEYS.iter().position(|&k| k as char == key) {
            if pos < list.entries.len() {
                screen = boot::boot_index(screen, &list, clip, tps, pos)?;
                return Ok(screen);
            }
        }
    }
}

/// 弹窗屏显（文字区；条目行与串口同一格式，见 `boot::entry_line`）。
fn draw_popup(screen: &mut Screen, list: &crate::boot::entries::BootList) {
    let _ = screen.clear();
    screen.reset_cursor();
    screen.draw_str("== Boot options ==");
    for w in &list.warnings {
        screen.draw_str(&format!("! {}", w));
    }
    if list.entries.is_empty() {
        screen.draw_str("no bootable entries.");
    }
    for i in 0..list.entries.len() {
        screen.draw_str(&entry_line(list, i));
    }
    if list.entries.len() > KEYS.len() {
        screen.draw_str(&format!(
            "! {} entries beyond key range, not selectable",
            list.entries.len() - KEYS.len()
        ));
    }
    screen.draw_str("");
    screen.draw_str("[p] player mode (diag menu)");
    screen.draw_str("[f] firmware setup (reboot)");
    screen.draw_str("[s] shutdown   [r] reboot");
    screen.draw_str("[q] back to wallpaper  [Enter] default");
}

/// 重启进固件设置（BootToFwUi）：置 `OsIndications` bit0 后冷重启。
/// 固件不支持（只读/无变量服务）就报错留下，不 halt。
fn boot_to_firmware(screen: &mut Screen) {
    use uefi::runtime::{VariableAttributes, VariableVendor, set_variable};
    log::warn!("popup: firmware setup, resetting...");
    let name = cstr16!("OsIndications");
    // EFI_OS_INDICATIONS_BOOT_TO_FW_UI（bit0），小端 u64。
    let data = 1u64.to_le_bytes();
    let attrs = VariableAttributes::NON_VOLATILE
        | VariableAttributes::BOOTSERVICE_ACCESS
        | VariableAttributes::RUNTIME_ACCESS;
    match set_variable(&name, &VariableVendor::GLOBAL_VARIABLE, attrs, &data) {
        Ok(()) => uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None),
        Err(e) => {
            screen.draw_str(&format!("firmware setup unsupported: {:?}", e.status()));
            log::warn!("popup: firmware setup failed: {:?}", e.status());
            pause_return(screen);
        }
    }
}
