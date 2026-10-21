//! 动态壁纸（P4）：播放器 `Clip` 的薄调用层，不自研解码。
//!
//! 片源格式（`[wallpaper] format`，默认 raw；GUI 只放行 qois/raw 双按钮）：
//! - `raw`：`wallpaper/frameNNNN.raw`，BGRA 直存，零解码直搬；
//! - `qois`：`wallpaper/wall.qois`（与视频同格式），开机预解码；
//! - 手写 `hevc`/`vp9`/`h263`/`mpeg2v`/`theora` 进壁纸：暂不支持，警告后无壁纸
//!   （播放器侧 hevc/vp9 已接，壁纸侧另立项；后继三家两边都后继）。
//! 布局/循环语义不变：A 菜单背景播行区，B 引导过渡播全屏。
//! 目录枚举/解码全部复用 `video::decoder::Clip`（`uefi-rs` 无 walker 的部分，
//! `Clip::from_raw_dir` 内自研 20 行，与播放器同一套）。

use crate::config::{self, MediaFormat};
use crate::error::Result;
use crate::fs::Fs;
use crate::video::decoder::Clip;
use uefi::cstr16;

/// 壁纸帧几何（raw 定死，和视频同分辨率，免缩放分支；qois 以文件头为准，
/// 非 1920x1080 的现模式跳过壁纸，见调用方）。
pub const WP_W: usize = 1920;
pub const WP_H: usize = 1080;
const FRAME_BYTES: usize = WP_W * WP_H * 4;

/// 菜单文本区保留高度（行）：标题 + 全部菜单项 + r/提示 + 余量。
/// `ITEMS.len()` 是 const 可求值，菜单加项自动跟。
pub const MENU_LINES: usize = crate::diag::registry::ITEMS.len() + 4;
/// 壁纸行区起点（菜单文字之下，18px/行 + 8px 间隙）。
pub const WP_TOP: usize = MENU_LINES * 18 + 8;

/// 入菜单/子菜单时调：开关关 → `None`；开 → 按格式预载（`preload` 上限，
/// 0 = 不封顶；缺目录/无有效帧不报错，返回 `None`）。
/// 预载帧数打串口（矩阵靠它断言 0 拉满与封顶行为）。
pub fn load() -> Result<Option<Clip>> {
    if !config::wallpaper_enabled() {
        return Ok(None);
    }
    let mut fs = Fs::new()?;
    // 0 = 不封顶（和播片一样拉满；`from_raw_dir` 的 `take` 与 qois 的
    // `truncate` 对 MAX 都是无操作，长动画注意内存）。
    let want = match config::wallpaper_preload() {
        0 => usize::MAX,
        n => n as usize,
    };
    let clip = match config::wallpaper_format() {
        MediaFormat::Raw => {
            Clip::from_raw_dir(&mut fs, "wallpaper", want, FRAME_BYTES, "wallpaper")
        }
        MediaFormat::Qois => {
            let file = match fs.open_file(cstr16!("wallpaper\\wall.qois")) {
                Ok(f) => f,
                // 缺片源是正常情况：静默无壁纸（开关开着也只是没片源）。
                Err(_) => return Ok(None),
            };
            Clip::from_qois_file(file, "wallpaper")
        }
        // 壁纸侧未接视频编解码（手写格式进来不断解析，警告后无壁纸）。
        other => {
            log::warn!("wallpaper: format {:?} not supported, no wallpaper", other);
            return Ok(None);
        }
    };
    let mut clip = match clip {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    // qois 整文件预解码后按 preload 截断（raw 在目录枚举时已封顶）。
    if config::wallpaper_format() == MediaFormat::Qois && clip.len() > want {
        clip.frames.truncate(want);
    }
    if clip.is_empty() {
        return Ok(None);
    }
    log::info!(
        "wallpaper: preloaded {} frames (preload {})",
        clip.len(),
        config::wallpaper_preload()
    );
    Ok(Some(clip))
}
