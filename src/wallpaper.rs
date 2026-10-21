//! 动态壁纸（P4）：播放器 `Clip` 的薄调用层 + 画廊拼接，不自研解码。
//!
//! 画廊模型（`[wallpaper]` enable/loop/fps/preload/popup_freeze + `[[wallpaper.items]]`）：
//! 每项按路径自动检测（与播放列表同一 `detect_clip_format`），恒 eager 预解码
//! （随机访问循环，以 tag 裁决忽略每格式 fetch 覆盖，见 `feed_for`）；
//! 各项帧几何必须一致（首项定，后继异形整项跳过 + warn）；
//! `preload` 按项封顶（0 = 不封顶；单帧 1920x1080x4 ≈ 8MB，内存兜底）；
//! 全拼成一个 `Clip`（调用方 `frame(i)`/`len` 零改；B 过渡长度即全集长度）。
//! 循环语义：`loop_single` 只取首项（重播当前项），`loop_list` 全拼回绕，
//! `once` 全拼 + 调用方末帧保持（`next_gallery_index`）；
//! 关/空画廊/全坏 → `None`（静默无壁纸，装饰不断流程）。
//! 目录枚举/解码全部复用 `video::decoder::Clip`（`uefi-rs` 无 walker 的部分，
//! `Clip::from_raw_dir` 内自研 20 行，与播放器同一套）。
//!
//! 新串口标记（矩阵卡点用）：
//! `wallpaper: gallery items={n} loop={mode} preloaded {m} frames (preload {p})`
//! （n = 画廊项数，m = 拼接帧数，p = 按项封顶值）。

use crate::config::{self, LoopMode};
use crate::error::Result;
use crate::fs::Fs;
use crate::video::decoder::Clip;
use crate::video::play::{detect_clip_format, load_item_clip};
use alloc::vec::Vec;
use uefi::proto::console::gop::BltPixel;

/// 壁纸帧几何（raw 定死，和视频同分辨率，免缩放分支；qois 以文件头为准，
/// 非 1920x1080 的现模式跳过壁纸，见调用方）。
pub const WP_W: usize = 1920;
pub const WP_H: usize = 1080;

/// 菜单文本区保留高度（行）：标题 + 全部菜单项 + r/提示 + 余量。
/// `ITEMS.len()` 是 const 可求值，菜单加项自动跟。
pub const MENU_LINES: usize = crate::diag::registry::ITEMS.len() + 4;
/// 壁纸行区起点（菜单文字之下，18px/行 + 8px 间隙）。
pub const WP_TOP: usize = MENU_LINES * 18 + 8;

/// 画廊步进（idle 待机/弹窗行区/菜单背景共用）：list 回绕，once 到末帧停留
/// （`len == 0` 兜底 0；single 画廊恒单项集，回绕即重播）。
pub fn next_gallery_index(idx: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    match config::wallpaper_loop() {
        LoopMode::Once => (idx + 1).min(len - 1),
        _ => (idx + 1) % len,
    }
}

/// 入菜单/子菜单时调：开关关 → `None`；开 → 逐项检测加载并拼接（`preload` 按项封顶，
/// 0 = 不封顶；缺目录/坏项跳过 + warn）；空画廊/全坏不报错，返回 `None`。
/// 预载帧数打串口（矩阵靠它断言项数与封顶行为）。
pub fn load() -> Result<Option<Clip>> {
    if !config::wallpaper_enabled() {
        return Ok(None);
    }
    let total = config::wallpaper_len();
    if total == 0 {
        return Ok(None);
    }
    let mut fs = Fs::new()?;
    let mode = config::wallpaper_loop();
    // 单曲只取首项（重播当前项；调用方回绕即重播，全拼形状不变）。
    let take = match mode {
        LoopMode::LoopSingle => 1.min(total),
        _ => total,
    };
    let cap = config::wallpaper_preload() as usize;
    let mut frames: Vec<Vec<BltPixel>> = Vec::new();
    let mut geom: Option<(usize, usize)> = None;
    for i in 0..take {
        let path = match config::wallpaper_path(i) {
            Some(p) => p,
            None => continue,
        };
        let fmt = match detect_clip_format(&path) {
            Some(f) => f,
            None => {
                log::warn!("playlist: skip {} (unknown format or probe failed)", path);
                continue;
            }
        };
        let mut clip = match load_item_clip(&mut fs, &path, fmt, "wallpaper", true) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("playlist: skip {} ({:?})", path, e);
                continue;
            }
        };
        // 按项封顶（0 = 不封顶，长动画注意内存）。
        if cap > 0 && clip.len() > cap {
            clip.frames.truncate(cap);
        }
        if clip.is_empty() {
            continue;
        }
        // 同一画廊几何必须一致（混入异形整项跳过，不撕裂后续帧定位）。
        match geom {
            None => geom = Some((clip.w, clip.h)),
            Some(g) if g != (clip.w, clip.h) => {
                log::warn!(
                    "wallpaper: gallery skip {} (geometry {}x{} != {}x{})",
                    path,
                    clip.w,
                    clip.h,
                    g.0,
                    g.1
                );
                continue;
            }
            _ => {}
        }
        frames.append(&mut clip.frames);
    }
    if frames.is_empty() {
        return Ok(None);
    }
    let (w, h) = geom.unwrap_or((WP_W, WP_H));
    let m = frames.len();
    log::info!(
        "wallpaper: gallery items={} loop={} preloaded {} frames (preload {})",
        total,
        mode.as_str(),
        m,
        config::wallpaper_preload()
    );
    Ok(Some(Clip::eager(w, h, frames)))
}
