//! `video::loops`：单线程播放循环（统一控制面，无旁路）。
//! `blt_loop`/`direct_loop` 走现行 `Clip`（五档 feed 全经此：clip 全量预解码 /
//! staged 显式三段 / stream 每格式窗边播边解 / zero_copy 单复用缓冲即时解码 /
//! single 零缓存逐帧重解）；旧 `[play] pipeline` 7 档已删（映射见 `docs/player.md`）：
//! staged->feed=staged+render.blt、zero_copy->feed=zero_copy+render.blt、
//! stream->feed=stream+render.blt（旧 stream_loop 无 drop，新 pace_or_drop 至多丢 2/帧；
//! soak 用 fps=max 对齐）、single->feed=single+render.blt、
//! legacy_blt->clip+eager+render.blt、legacy_direct->clip+eager+render.direct+workers=bsp_only、
//! legacy_u64->clip+eager+render.direct+workers=bsp_only+accel.level=scalar。

use super::decoder::Clip;
use super::{pace_or_drop, rdtsc};
use crate::error::Result;
use crate::graphics::Screen;

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
            None => video.rewind(),
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
            None => video.rewind(),
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}
