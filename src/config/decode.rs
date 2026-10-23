//! `[decode.<fmt>]` 每格式解码表运行时值（`src/config` 的 decode 半；
//! `types::DecodeTable` 定 schema，本文件定生效口径，P2 起直读 `runtime`）。
//!
//! - `threads`：`bsp_only` 安全默认；`all` 在 `qois`/`raw` 立即兑现，
//!   有状态软解解析但告警仍 `bsp_only`（每格式安全管线落地前）；
//! - `zero_copy` / `streaming`：单选开关，默认 false（`feed` 照常走对应复用/窗口，
//!   本项为叠加 hint）；
//! - `buffers`：滑杆下标，0 跟随 `tune.window` 全局默认，1-32 显式覆盖每格式。
//! - `fetch`：每格式供片覆盖（None = 继承全局 `[play] feed`；显式值只覆盖本格式，壁纸恒 clip 忽略覆盖）。

use super::{DecodeEntry, DecodeThreads, FeedMode, MediaFormat};

/// 取某格式条目（P2 起读 `runtime().decode`；`DecodeEntry` 为 Copy）。
fn entry(fmt: MediaFormat) -> DecodeEntry {
    let d = &super::runtime().decode;
    match fmt {
        MediaFormat::Qois => d.qois,
        MediaFormat::Raw => d.raw,
        MediaFormat::Hevc => d.hevc,
        MediaFormat::Vp9 => d.vp9,
        MediaFormat::H263 => d.h263,
        MediaFormat::Mpeg2v => d.mpeg2v,
        MediaFormat::Theora => d.theora,
        MediaFormat::Avc => d.avc,
    }
}

/// 是否有状态软解（顺序解码，其余三项 yes；`qois`/`raw` 非状态）。
fn is_stateful_fmt(fmt: MediaFormat) -> bool {
    !matches!(fmt, MediaFormat::Qois | MediaFormat::Raw)
}

fn decode_key(fmt: MediaFormat) -> &'static str {
    match fmt {
        MediaFormat::Qois => "qois",
        MediaFormat::Raw => "raw",
        MediaFormat::Hevc => "hevc",
        MediaFormat::Vp9 => "vp9",
        MediaFormat::H263 => "h263",
        MediaFormat::Mpeg2v => "mpeg2v",
        MediaFormat::Theora => "theora",
        MediaFormat::Avc => "avc",
    }
}

/// 解码线程档（`[decode.<fmt>] threads`）：`bsp_only` 安全默认；
/// `all` 在 `qois`/`raw` 立即兑现（帧级并行可行），有状态软解解析但告警
/// 仍 `bsp_only`（每格式安全管线落地前）。
pub fn decode_threads(fmt: MediaFormat) -> DecodeThreads {
    let want = entry(fmt).threads;
    if want != DecodeThreads::All {
        return DecodeThreads::BspOnly;
    }
    if is_stateful_fmt(fmt) {
        log::warn!("decode: {:?} threads all deferred (staying bsp_only)", fmt);
        return DecodeThreads::BspOnly;
    }
    log::info!(
        "decode: fmt={} threads=all honored (frame-parallel safe)",
        decode_key(fmt)
    );
    DecodeThreads::All
}

/// 解码零拷开关（`[decode.<fmt>] zero_copy`，默认 false）。
pub fn decode_zero_copy(fmt: MediaFormat) -> bool {
    entry(fmt).zero_copy
}

/// 解码流式开关（`[decode.<fmt>] streaming`，默认 false）。
pub fn decode_streaming(fmt: MediaFormat) -> bool {
    entry(fmt).streaming
}

/// 每格式解码窗下标（`[decode.<fmt>] buffers` 原始值 0-32：0 跟随全局，
/// 1-32 显式覆盖；已钳制）。
pub fn decode_buffers(fmt: MediaFormat) -> u32 {
    entry(fmt).buffers.min(32)
}

/// 每格式生效解码窗（帧数）：`buffers = 0` 继承全局 `tune.window`（1-32），
/// 显式 1-32 按格式覆盖；`tune.window` 仍是全局默认。
pub fn decode_window(fmt: MediaFormat) -> usize {
    match decode_buffers(fmt) {
        0 => super::tune_window().clamp(1, 32).max(1) as usize,
        n => n.clamp(1, 32).max(1) as usize,
    }
}

/// RAW-GEOM：`[decode.raw]` 生效几何（已钳 1..=4096；缺键 1920x1080 照旧）。
/// 帧字节 = `w*h*4`（BGRA 直存）；几何 preset + mp_slots 步幅同取此处。
pub fn decode_raw_wh() -> (usize, usize) {
    let d = super::runtime().decode.raw;
    let w = d.width.clamp(1, 4096) as usize;
    let h = d.height.clamp(1, 4096) as usize;
    (w.max(1), h.max(1))
}

/// 每格式供片覆盖（`[decode.<fmt>] fetch`，默认 None = 继承全局 `[play] feed`；
/// 显式 clip/staged/stream/zero_copy/single 只覆盖本格式）。
/// 无别名，未知键忽略（serde 默认）；壁纸恒 clip，调用方以 tag 裁决忽略本项。
pub fn decode_fetch(fmt: MediaFormat) -> Option<FeedMode> {
    entry(fmt).fetch
}

/// 有效供片：每格式 `fetch` 覆盖全局 `[play] feed`（None 则继承全局）。
/// 解码包装按此裁决懒/全量形态；`play.rs` 按此记 `video: feed=` 行。
pub fn effective_feed(fmt: MediaFormat) -> FeedMode {
    decode_fetch(fmt).unwrap_or(super::play_feed())
}

/// `effective_feed` 别名（供 decoder 垫片/旧名调用，同值）。
pub fn decode_effective_feed(fmt: MediaFormat) -> FeedMode {
    effective_feed(fmt)
}
