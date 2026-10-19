//! BLS 引导子菜单（P1，全功能对齐中）：枚举本卷 `loader/entries` +
//! 多卷自动发现（BLS/UKI/Windows/Linux/Shell，见 `detect.rs`），选 PE 项
//! chainload，选 Linux 项直启（kernel + initrd + options + devicetree）。
//!
//! - `Entry::Exclusive`：handoff 前必须释放 GOP 独占，否则下个镜像打不开 GOP。
//!   这与路线图 P1 原文“Shared 形态”有偏离——Shared 借屏不释放，handover 会楔死，
//!   故用 Exclusive（菜单负责 drop/重建），特此说明。
//! - P4-B：选 PE 项后、chainload 前播 N 帧壁纸过渡（全屏，开时才播）。

pub mod chainload;
pub mod detect;
pub mod entries;

use alloc::format;
use alloc::string::String;

use self::chainload::chainload_efi;
use self::entries::{BootKind, BootList, enumerate};
use crate::diag::common::{flush_keys, pause_return, wait_key_ex};
use crate::error::Result;
use crate::graphics::{Screen, calibrate_tps, tsc};
use crate::wallpaper::{self, WallpaperSet};

/// 子菜单按键：36 个（BLS 项再多也够，超了截断并 warning）。
const KEYS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

pub fn run() -> Result {
    let mut screen = Screen::reacquire()?;
    let mut list = enumerate()?;
    // 多卷自动发现（`[boot] auto` 总开关；关 = 只列本卷 BLS，老行为）。
    if crate::config::boot_auto() {
        let (detected, mut warnings) = detect::detect();
        list.warnings.append(&mut warnings);
        detect::append_unique(&mut list, detected);
    }
    // 条目数 + 明细标记（串口可断言）：QEMU 黑盒矩阵靠它同步按键 + 断言种类
    //（子菜单屏显-only）。只打一次，`draw_boot_menu` 重画不再刷。
    log::warn!("boot: {} entries", list.entries.len());
    for (i, e) in list.entries.iter().enumerate() {
        let key = if i < KEYS.len() { KEYS[i] as char } else { '?' };
        log::warn!("boot: [{}] {} <{}>", key, e.title, e.kind.kind_str());
    }
    // B 过渡用壁纸预载集（和 A 同格式；缺片源/关开关即 None，跳过过渡）。
    let wp = wallpaper::load().unwrap_or(None);
    let tps = if wp.is_some() { calibrate_tps() } else { 1 };

    if list.entries.is_empty() {
        let _ = screen.clear();
        screen.reset_cursor();
        screen.draw_str("== Boot entries (BLS) ==");
        for w in &list.warnings {
            screen.draw_str(w);
        }
        screen.draw_str("no bootable entries.");
        // 串口标记（QEMU 黑盒矩阵靠它同步按键，屏显测不到）。
        log::warn!("boot: no entries, press any key");
        pause_return(&mut screen);
        return Ok(());
    }

    loop {
        draw_boot_menu(&mut screen, &list);
        flush_keys();
        let (key, _) = wait_key_ex();
        if key == 'q' {
            return Ok(());
        }
        if key == '\r' || key == '\n' {
            if let Some(i) = list.default_index {
                screen = boot_index(screen, &list, &wp, tps, i)?;
            }
            continue;
        }
        if let Some(pos) = KEYS.iter().position(|&k| k as char == key) {
            if pos < list.entries.len() {
                screen = boot_index(screen, &list, &wp, tps, pos)?;
            }
        }
    }
}

fn draw_boot_menu(screen: &mut Screen, list: &BootList) {
    let _ = screen.clear();
    screen.reset_cursor();
    screen.draw_str("== Boot entries (BLS) ==");
    for w in &list.warnings {
        screen.draw_str(&format!("! {}", w));
    }
    for (i, e) in list.entries.iter().enumerate() {
        let key = if i < KEYS.len() { KEYS[i] as char } else { '?' };
        let tries = match e.tries_left {
            Some(t) => format!(" (tries {})", t),
            None => String::new(),
        };
        let def = if Some(i) == list.default_index {
            " [default]"
        } else {
            ""
        };
        let kind = match &e.kind {
            BootKind::Efi { .. } => "efi",
            BootKind::Linux { .. } => "linux*",
        };
        screen.draw_str(&format!("[{}] {} <{}>{}{}", key, e.title, kind, tries, def));
    }
    if list.entries.len() > KEYS.len() {
        screen.draw_str(&format!(
            "! {} entries beyond key range, not selectable",
            list.entries.len() - KEYS.len()
        ));
    }
    screen.draw_str("[q] back  [Enter] default");
}

/// 值传递拿走 Screen：`drop` 即释放 GOP 独占，chainload 才能干净交权；
/// 出错/镜像返回时重建一个交回，子菜单继续。
fn boot_index(
    screen: Screen,
    list: &BootList,
    wp: &Option<WallpaperSet>,
    tps: u64,
    i: usize,
) -> Result<Screen> {
    let entry = &list.entries[i];
    log::warn!("boot: starting [{}] {}", entry.file_name, entry.title);
    // boot counting：尝试即递减 tries（失败只记 warning，不挡启动）。
    {
        let mut fs = match crate::fs::Fs::open(volume_of(entry)) {
            Ok(f) => f,
            Err(e) => {
                // 卷打不开：记一笔，继续启动（读文件时还会再报错，不丢信息）。
                log::warn!("boot: counter fs open failed: {:?}", e);
                return boot_entry(screen, list, wp, tps, i);
            }
        };
        if let Some(w) = crate::boot::entries::consume_try(&mut fs, entry) {
            log::warn!("boot: {}", w);
        }
    }
    boot_entry(screen, list, wp, tps, i)
}

/// 卷选择：条目自带卷用条目的（跨盘），`None` 走本卷（老行为）。
fn volume_of(entry: &crate::boot::entries::BootEntry) -> Option<uefi::Handle> {
    match &entry.kind {
        BootKind::Efi { volume, .. } => *volume,
        BootKind::Linux { volume, .. } => *volume,
    }
}

fn boot_entry(
    mut screen: Screen,
    list: &BootList,
    wp: &Option<WallpaperSet>,
    tps: u64,
    i: usize,
) -> Result<Screen> {
    let entry = &list.entries[i];
    let volume = volume_of(entry);
    match &entry.kind {
        BootKind::Linux {
            kernel,
            initrd,
            options,
            devicetree,
            ..
        } => {
            // Linux 直启（P1 全功能对齐新增）：kernel + initrd 拼接 + options +
            // devicetree，语义与上游 `actions/chainload.rs` 一致（initrd 经
            // `LINUX_EFI_INITRD_MEDIA_GUID` 交接，stubble 占位 options）。
            let saved = (
                String::from(kernel),
                initrd.clone(),
                options.clone(),
                devicetree.clone(),
            );
            drop(screen);
            match chainload::chainload_linux(
                &saved.0,
                &saved.1,
                saved.2.as_deref(),
                saved.3.as_deref(),
                volume,
            ) {
                Err(e) => {
                    let mut screen = Screen::reacquire()?;
                    screen.draw_str(&format!("boot failed: {:?}", e));
                    log::warn!("boot failed: {:?}", e);
                    pause_return(&mut screen);
                    Ok(screen)
                }
                Ok(()) => {
                    let mut screen = Screen::reacquire()?;
                    screen.draw_str("entry returned, back to boot menu.");
                    pause_return(&mut screen);
                    Ok(screen)
                }
            }
        }
        BootKind::Efi { path, options, .. } => {
            // P4-B：chainload 前播 N 帧过渡（分辨率不对/无片源则跳过，不挡引导）。
            play_transition(&mut screen, wp, tps);
            let saved = (String::from(path), options.clone());
            // GOP 独占必须留在这里，不能带过 handoff。
            drop(screen);
            match chainload_efi(&saved.0, saved.1.as_deref(), volume) {
                Err(e) => {
                    let mut screen = Screen::reacquire()?;
                    screen.draw_str(&format!("boot failed: {:?}", e));
                    // 屏显 + 串口双写：无头 QEMU 下屏显看不见（矩阵曾因此哑巴）。
                    log::warn!("boot failed: {:?}", e);
                    pause_return(&mut screen);
                    Ok(screen)
                }
                Ok(()) => {
                    // 被起镜像主动返回：重建屏显，回子菜单。
                    let mut screen = Screen::reacquire()?;
                    screen.draw_str("entry returned, back to boot menu.");
                    pause_return(&mut screen);
                    Ok(screen)
                }
            }
        }
    }
}

/// P4-B 过渡：`frames` 封顶（0 = 全播预载集），按壁纸 fps 限速。
/// 非 1920x1080 现显模式跳过（帧几何定死，不缩放）。
fn play_transition(screen: &mut Screen, wp: &Option<WallpaperSet>, tps: u64) {
    let Some(set) = wp else {
        return;
    };
    if set.is_empty() {
        return;
    }
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    if (w as usize, h as usize) != (wallpaper::WP_W, wallpaper::WP_H) {
        log::warn!(
            "wallpaper: B transition skipped (mode {}x{}, need 1920x1080)",
            w,
            h
        );
        return;
    }
    let cap = crate::config::wallpaper_frames() as usize;
    let n = if cap == 0 {
        set.len()
    } else {
        cap.min(set.len())
    };
    let budget = tps.max(1) / crate::config::wallpaper_fps().max(1) as u64;
    screen.draw_str(&format!("wallpaper: transition {} frames...", n));
    for i in 0..n {
        let fstart = tsc();
        if screen
            .upload_frame(wallpaper::WP_W, wallpaper::WP_H, set.frame(i))
            .is_err()
        {
            break;
        }
        while tsc().saturating_sub(fstart) < budget {
            core::hint::spin_loop();
        }
    }
}
