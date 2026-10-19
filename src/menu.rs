//! 开机菜单：默认停留等待按键，按 0-7 进对应项，r 重启。
//!
//! 错误策略：
//! - 播放视频（生产路径）失败 → [`handle_fatal`]，和重构前 `main` 语义完全一致；
//! - 诊断项失败 → 可返回式报错显示，按任意键回菜单；
//! - [`handle_fatal`] 本体（`error.rs`）一字不动。

use core::time::Duration;

use uefi::boot::stall;

use crate::diag::common::{flush_keys, pause_return, poll_key};
use crate::diag::registry::{Entry, ITEMS};
use crate::error::{handle_fatal, NyaStatus, Result};
use crate::graphics::{Screen, calibrate_tps, tsc};
use crate::video::decoder::Clip;
use crate::wallpaper::{self, WP_H, WP_TOP, WP_W};

fn draw_menu(screen: &mut Screen) {
    let _ = screen.clear();
    screen.reset_cursor();
    screen.draw_str("== UEFI Diag Menu ==");
    for item in ITEMS {
        screen.draw_str(&alloc::format!("[{}] {} - {}", item.key, item.name, item.desc));
    }
    screen.draw_str("[r] reboot");
    screen.draw_str("Press key to select:");
}

/// 诊断项的可返回式报错：显示错误，等任意键回菜单（不 halt）。
fn show_recoverable(screen: &mut Screen, err: NyaStatus) {
    let _ = screen.clear();
    screen.reset_cursor();
    screen.draw_str(&alloc::format!("ERROR: {:?}", err));
    pause_return(screen);
}

fn run_shared(screen: &mut Screen, f: fn(&mut Screen) -> Result, fatal: bool) {
    let _ = screen.clear();
    screen.reset_cursor();
    match f(screen) {
        Ok(()) => pause_return(screen),
        // 生产路径保持原语义：致命屏显（error.rs 原样保留）。
        Err(e) if fatal => handle_fatal(e, screen),
        Err(e) => show_recoverable(screen, e),
    }
}

/// 开机预载菜单背景：开关关/无片源/分辨率不对都返回 `None`。
fn load_background(screen: &mut Screen) -> Option<WpState> {
    let set = wallpaper::load().unwrap_or(None)?;
    if set.is_empty() {
        return None;
    }
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    if (w as usize, h as usize) != (WP_W, WP_H) {
        log::warn!("wallpaper: menu bg skipped (mode {}x{}, need 1920x1080)", w, h);
        return None;
    }
    let tps = calibrate_tps();
    let budget = tps.max(1) / crate::config::wallpaper_fps().max(1) as u64;
    Some(WpState {
        set,
        idx: 0,
        last: tsc(),
        budget,
    })
}

/// 阻塞等按键，空闲时按帧预算在文字区下方播壁纸（`WP_TOP` 以下行区，
/// 菜单文字不重画——`draw_str` 逐像素 `VideoFill` 跟不上帧率）。
/// 无壁纸时等价旧 `wait_key_ex`（轮询 + 短 stall）。
fn wait_key_or_animate(screen: &mut Screen, wp: &mut Option<WpState>) -> (char, u16) {
    loop {
        if let Some((key, raw)) = poll_key() {
            return (key, raw);
        }
        let mut kill_wp = false;
        if let Some(st) = wp {
            let now = tsc();
            if now.saturating_sub(st.last) >= st.budget {
                st.last = now;
                // 行区 paint 失败（如 GOP 抖动）就停背景，不炸菜单。
                if screen
                    .upload_frame_region(WP_W, WP_H, WP_TOP, st.set.frame(st.idx))
                    .is_ok()
                {
                    st.idx += 1;
                } else {
                    kill_wp = true;
                }
            }
        }
        if kill_wp {
            *wp = None;
        }
        stall(Duration::from_millis(5));
    }
}

/// 兜底比对：部分固件把扫描码/高位塞进 unicode_char（如数字键给 0x8038
/// 而非 0x0038），直接比字符就全线 Unknown。这里再拿低 8 位/低 7 位试一次。
fn key_match(want: char, key: char, raw: u16) -> bool {
    want == key
        || want == ((raw & 0xFF) as u8 as char).to_ascii_lowercase()
        || want == ((raw & 0x7F) as u8 as char).to_ascii_lowercase()
}

/// 壁纸 idle 状态：预载剪辑（`Clip`，双格式统一）+ 下一帧序号 + 上一帧 ticks + 帧预算。
struct WpState {
    set: Clip,
    idx: usize,
    last: u64,
    budget: u64,
}

pub fn run_menu(mut screen: Screen) -> ! {
    // P4-A：菜单背景预载（关/无片源/分辨率不对 → None，纯静态菜单）。
    let mut wp = load_background(&mut screen);
    let mut wp_warned = false;
    // 就绪标记（串口可断言）：QEMU 黑盒矩阵靠它定发送按键的时机
    // （早发的键会被下面的 flush_keys 当陈旧输入排空）。
    log::info!("menu: ready (press key)");
    loop {
        draw_menu(&mut screen);
        // 开着但没片源：提示一次（随后重画不再刷，避免挤占行区预留）。
        if crate::config::wallpaper_enabled() && wp.is_none() && !wp_warned {
            screen.draw_str("wallpaper: enabled but no frames, static menu.");
            wp_warned = true;
        }
        flush_keys();
        let (key, raw) = wait_key_or_animate(&mut screen, &mut wp);

        if key_match('r', key, raw) {
            uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None);
        }

        let mut matched = false;
        for item in ITEMS {
            if key_match(item.key, key, raw) {
                matched = true;
                match item.run {
                    Entry::Shared(f) => {
                        run_shared(&mut screen, f, item.fatal_on_error);
                        // 子菜单返回标记（串口可断言；QEMU 黑盒矩阵靠它同步按键）。
                        log::info!("menu: back from {}", item.key);
                    }
                    Entry::Exclusive(f) => {
                        // 该 demo 需要自己独占 GOP（强制切显示模式）。
                        // 先释放 Screen 再跑，跑完重建 Screen 回菜单。
                        // 注意：这里不清屏——demo 画面要留给用户看，
                        // 提示行打在底部。
                    drop(screen);
                    let result = f();
                    screen = Screen::reacquire().expect("Failed to re-create screen");
                    screen.cursor_to_bottom();
                    log::info!("menu: back from {}", item.key);
                        match result {
                            Ok(()) => pause_return(&mut screen),
                            Err(e) => show_recoverable(&mut screen, e),
                        }
                    }
                }
                break;
            }
        }

        if !matched {
            // 带上原始键值：固件若把扫描码塞进 unicode_char，这里能看出来。
            screen.draw_str(&alloc::format!(
                "Unknown key: code U+{:04X} ('{}') - try again.",
                raw, key
            ));
            stall(Duration::from_millis(500));
        }
    }
}
