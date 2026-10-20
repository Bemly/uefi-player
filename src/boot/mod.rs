//! 引导项数据 + 交权（P1，全功能对齐中）：枚举本卷 `loader/entries` +
//! 多卷自动发现（BLS/UKI/Windows/Linux/Shell/OpenCore，见 `detect.rs`），选 PE 项
//! chainload，选 Linux 项直启（kernel + initrd + options + devicetree）。
//!
//! UI 在 `idle.rs` 的启动选项弹窗（条目直接列出，数字键直选）；
//! 本模块只做枚举 + 交权，不画菜单。
//!
//! - 交权前必须释放 GOP 独占（`drop(Screen)`），否则下个镜像打不开 GOP。
//! - P4-B：选 PE 项后、chainload 前播 N 帧壁纸过渡（全屏，开时才播）。

pub mod chainload;
pub mod detect;
pub mod entries;

use alloc::format;
use alloc::string::String;

use self::chainload::chainload_efi;
use self::entries::{BootKind, BootList, enumerate};
use crate::diag::common::pause_return;
use crate::error::Result;
use crate::graphics::{Screen, tsc};
use crate::video::decoder::Clip;
use crate::wallpaper::{self};

/// 启动选项弹窗的条目按键（`idle.rs` 复用；`p/f/s/r` 留给固定项，`q` 返回，
/// `Enter` 跑默认——调用方先判固定键，剩下的才按此表查条目）。
/// 32 档（0-9a-z 去掉 p/f/s/r，超了截断并 warning）。
pub const KEYS: &[u8] = b"0123456789abcdeghijklmnoqtuvwxyz";

/// 枚举启动项（本卷 BLS + 多卷自动发现），打串口标记（矩阵靠它同步按键）。
/// `idle.rs` 每次开弹窗调一次，保证插拔 U 盘后列表新鲜。
pub fn list() -> Result<BootList> {
    let mut list = enumerate()?;
    // 多卷自动发现（`[boot] auto` 总开关；关 = 只列本卷 BLS，老行为）。
    if crate::config::boot_auto() {
        let (detected, mut warnings) = detect::detect();
        list.warnings.append(&mut warnings);
        detect::append_unique(&mut list, detected);
    }
    // 条目数 + 明细标记（串口可断言）：矩阵靠它同步按键 + 断言种类。
    log::warn!("boot: {} entries", list.entries.len());
    for (i, e) in list.entries.iter().enumerate() {
        let key = if i < KEYS.len() { KEYS[i] as char } else { '?' };
        log::warn!("boot: [{}] {} <{}>", key, e.title, e.kind.kind_str());
    }
    Ok(list)
}

/// 条目行文本（弹窗与串口同一格式；`?` = 超出按键范围，不可选）。
pub fn entry_line(list: &BootList, i: usize) -> String {
    let e = &list.entries[i];
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
    format!(
        "[{}] {} <{}>{}{}",
        key,
        e.title,
        e.kind.kind_str(),
        tries,
        def
    )
}

/// 值传递拿走 Screen：`drop` 即释放 GOP 独占，chainload 才能干净交权；
/// 出错/镜像返回时重建一个交回，调用方（弹窗）继续。
pub fn boot_index(
    screen: Screen,
    list: &BootList,
    wp: &Option<Clip>,
    tps: u64,
    i: usize,
) -> Result<Screen> {
    let entry = &list.entries[i];
    log::warn!("boot: starting [{}] {}", entry.file_name, entry.title);
    // boot counting：尝试即递减 tries（上游真改名；失败只记 warning，不挡启动）。
    if let Some(w) = crate::boot::entries::consume_try(entry) {
        log::warn!("boot: {}", w);
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
    wp: &Option<Clip>,
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
fn play_transition(screen: &mut Screen, wp: &Option<Clip>, tps: u64) {
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
