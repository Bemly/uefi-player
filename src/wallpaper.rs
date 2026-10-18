//! 动态壁纸（P4）：序列位图，不用 QOI。
//!
//! 轮子结论（见根 `AGENTS.md`「轮子优先」）：
//! - 搜过 `crates.io` 显示/视频方向：`qoi` 是压缩编解码轮（播放器 P3 在用），
//!   壁纸要的是“零解码、直搬显存”，压缩反而加 CPU 与首帧抖动，故不用任何轮子；
//!   格式就是 raw BGRA 直存，转制一行 ffmpeg（见 `docs/roadmap.md` P4）。
//! - 目录枚举复用 `Fs::list_dir`（`fs.rs` 自研 20 行，`uefi-rs` 无现成 walker）。
//!
//! 布局：U盘 `wallpaper/frameNNNN.raw`，每帧 1920x1080 BGRA（`BltPixel` 内存
//! 布局正好是 BGRA，零转换搬运）。`[wallpaper]` 见 `config.rs` 头注释。
//! - A 菜单背景：`run_menu` 空闲时在菜单文本区下方循环播（`WP_TOP` 以下行区，
//!   菜单文字不重画——`draw_str` 是逐像素 `VideoFill`，整屏重画跟不上帧率）。
//! - B 引导过渡：`boot::run` 在 chainload 前播 N 帧（全屏）。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::cstr16;
use uefi::data_types::CString16;
use uefi::proto::console::gop::BltPixel;

use crate::config;
use crate::error::{NyaStatus, Result};
use crate::fs::Fs;

/// 壁纸帧几何（首版定死，和视频同分辨率，免缩放分支）。
pub const WP_W: usize = 1920;
pub const WP_H: usize = 1080;
const FRAME_BYTES: usize = WP_W * WP_H * 4;

/// 菜单文本区保留高度（行）：标题 + 全部菜单项 + r/提示 + 余量。
/// `ITEMS.len()` 是 const 可求值，菜单加项自动跟。
pub const MENU_LINES: usize = crate::diag::registry::ITEMS.len() + 4;
/// 壁纸行区起点（菜单文字之下，18px/行 + 8px 间隙）。
pub const WP_TOP: usize = MENU_LINES * 18 + 8;

pub struct WallpaperSet {
    frames: Vec<Vec<BltPixel>>,
}

impl WallpaperSet {
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn frame(&self, i: usize) -> &[BltPixel] {
        &self.frames[i % self.frames.len().max(1)]
    }
}

/// 入菜单/子菜单时调：开关关 → `None`；开 → 预载（内存兜底 `preload` 上限）。
/// 缺目录/无有效帧不报错，返回 `None` + 警告行（壁纸是装饰，不 halt 菜单）。
pub fn load() -> Result<Option<WallpaperSet>> {
    if !config::wallpaper_enabled() {
        return Ok(None);
    }
    let mut fs = Fs::new()?;
    let entries = match fs.list_dir(cstr16!("wallpaper")) {
        Ok(e) => e,
        // 没有 wallpaper 目录是正常情况：静默无壁纸（开关开着也只是没片源）。
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => return Ok(None),
        Err(e) => {
            log::warn!("wallpaper: list failed: {:?}", e);
            return Ok(None);
        }
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(n, _)| n)
        .filter(|n| n.to_ascii_lowercase().ends_with(".raw"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Ok(None);
    }

    let want = config::wallpaper_preload().max(1) as usize;
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    for name in names.into_iter().take(want) {
        let path = format!("wallpaper\\{}", name);
        let cstr = match CString16::try_from(path.as_str()) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let bytes = match fs.read_file(&cstr) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        // 几何不对（非 1920x1080 BGRA）的帧跳过，不让半帧上屏。
        if bytes.len() != FRAME_BYTES {
            skipped += 1;
            continue;
        }
        frames.push(bytes_as_pixels(bytes));
    }
    if frames.is_empty() {
        // 全跳过也记一笔（曾静默，QEMU 实测靠它定位到测试帧尺寸写错）。
        log::warn!("wallpaper: no valid frames ({} skipped)", skipped);
        return Ok(None);
    }
    if skipped > 0 {
        log::warn!("wallpaper: skipped {} bad frame(s)", skipped);
    }
    Ok(Some(WallpaperSet { frames }))
}

/// BGRA 裸字节零拷贝 reinterpret 为 `&[BltPixel]` 的 owned 版。
/// `BltPixel` 内存布局就是 BGRA（`blue, green, red, reserved`，见 uefi 源码），
/// 对齐均为 1，`len % 4 == 0` 已由调用方保证。
fn bytes_as_pixels(bytes: Vec<u8>) -> Vec<BltPixel> {
    debug_assert_eq!(bytes.len() % 4, 0);
    let len = bytes.len() / 4;
    let cap = bytes.capacity() / 4;
    let ptr = bytes.as_ptr() as *mut BltPixel;
    core::mem::forget(bytes);
    unsafe { Vec::from_raw_parts(ptr, len, cap) }
}
