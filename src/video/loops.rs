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
//! 播放列表步进（本文件后半）：条目播完（`next_frame` 见 None）按
//! `play::next_playlist_index` 推进并重载（`advance_playlist`，逐项
//! `playlist: item` 行即重播证明；坏项已 warn 跳过，至多绕一圈）；
//! `once` 到头回 `Ok`（菜单返回），空列表走旧 rewind 语义（到此必非空，纯兜底）。

use super::decoder::Clip;
use super::play::{advance_playlist, need_eager_clip, playlist_cursor};
use super::{pace_or_drop, rdtsc};
use crate::error::Result;
use crate::graphics::Screen;

/// 条目播完推进（两循环共用）：有游标即按列表步进（无处可去回 true = 结束），
/// 无游标（空列表兜底）rewind 回 false。
fn playlist_step(video: &mut Clip) -> bool {
    match playlist_cursor() {
        Some(cur) => advance_playlist(video, cur, need_eager_clip()).is_none(),
        None => {
            video.rewind();
            false
        }
    }
}

pub(crate) fn blt_loop(
    screen: &mut Screen,
    mut video: Clip,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => {
                if playlist_step(&mut video) {
                    return Ok(());
                }
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
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => {
                if playlist_step(&mut video) {
                    return Ok(());
                }
            }
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}
