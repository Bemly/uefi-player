//! `video::loops`：单线程播放循环（统一控制面，无旁路）。
//! `blt_loop`/`direct_loop` 走现行 `Clip`（五档 feed 全经此：clip 全量预解码 /
//! staged 显式三段 / stream 每格式窗边播边解 / zero_copy 单复用缓冲即时解码 /
//! single 零缓存逐帧重解）；旧 `[play] pipeline` 7 档已删（映射见 `docs/player.md`）：
//! staged->feed=staged+render.blt、zero_copy->feed=zero_copy+render.blt、
//! stream->feed=stream+render.blt（旧 stream_loop 无 drop，新 pace_or_drop 至多丢 2/帧；
//! soak 用 fps=max 对齐）、single->feed=single+render.blt、
//! legacy_blt->clip+eager+render.blt、legacy_direct->clip+eager+render.direct+workers=bsp_only、
//! legacy_u64->clip+eager+render.direct+workers=bsp_only+accel.level=scalar。
//!
//! 单播语义（TUI 单路径化后）：本片播完回绕重播（`rewind`，与 `nano_pump`
//! 片尾同形）；列表步进/游标已删，不步进播放列表。
//!
//! `[display]`：每 clip 起播记一笔 `display:` marker（`present::note_clip_start`，
//! 源几何取 `Clip::w/h`，屏几何取 GOP；回绕后再记一笔）。上屏源几何
//! 同取 `Clip::w/h`（`width/height` 形参为旧 `VIDEO_W/H` 常量，1080p 时恒等，
//! 小尺寸片源以 clip 为准，不断 OOB）。

use super::decoder::Clip;
use super::{pace_or_drop, rdtsc};
use crate::error::Result;
use crate::graphics::Screen;

/// 起播标记（`display:` 行，矩阵卡点用）：源取 clip 几何，屏取 GOP 现模式。
fn note_display(screen: &mut Screen, video: &Clip) {
    let (dw, dh) = screen.get_gop().current_mode_info().resolution();
    crate::graphics::present::note_clip_start(video.w as u32, video.h as u32, dw as u32, dh as u32);
}

pub(crate) fn blt_loop(
    screen: &mut Screen,
    mut video: Clip,
    _width: usize,
    _height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    note_display(screen, &video);
    let mut drops = 0u64;
    loop {
        if crate::diag::common::poll_key().is_some() {
            return Ok(());
        }
        let fstart = rdtsc();
        let (sw, sh) = (video.w, video.h);
        match video.next_frame() {
            Some(frame) => screen.upload_frame(sw, sh, frame)?,
            None => {
                // 单播完回绕（列表步进已删；与 `nano_pump` 片尾同形）。
                video.rewind();
                note_display(screen, &video);
            }
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}

/// BSP 单核直写播放：与 `blt_loop` 同结构，只是走 CPU 直写显存
///（校准判单核更快，或用户关掉 MP 时用；`upload_frame` 按生效路径分发，
/// 这里 render 已定 direct，实际就是 `upload_direct`）。
pub(crate) fn direct_loop(
    screen: &mut Screen,
    mut video: Clip,
    _width: usize,
    _height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    note_display(screen, &video);
    let mut drops = 0u64;
    loop {
        if crate::diag::common::poll_key().is_some() {
            return Ok(());
        }
        let fstart = rdtsc();
        let (sw, sh) = (video.w, video.h);
        match video.next_frame() {
            Some(frame) => screen.upload_frame(sw, sh, frame)?,
            None => {
                // 单播完回绕（同上）。
                video.rewind();
                note_display(screen, &video);
            }
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}
