//! `[decode.<fmt>]` 每格式解码表运行时值（`src/config` 的 decode 半；
//! `types::DecodeTable` 定 schema，本文件定原子存储 + 生效口径）。
//!
//! - `threads`：`bsp_only` 安全默认；`all` 在 `qois`/`raw` 立即兑现，
//!   有状态软解解析但告警仍 `bsp_only`（每格式安全管线落地前）；
//! - `zero_copy` / `streaming`：单选开关，默认 false（`feed` 照常走对应复用/窗口，
//!   本项为叠加 hint）；
//! - `buffers`：滑杆下标，0 跟随 `tune.window` 全局默认，1-32 显式覆盖每格式。

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use super::{DecodeTable, DecodeThreads, MediaFormat};

/// 每格式线程档（0 = bsp_only 默认，1 = all 请求值；生效见 `decode_threads`）。
static DEC_THREADS_QOIS: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_RAW: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_HEVC: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_VP9: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_H263: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_MPEG2V: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_THEORA: AtomicU8 = AtomicU8::new(0);
static DEC_THREADS_AVC: AtomicU8 = AtomicU8::new(0);
/// 每格式零拷开关（0/1）。
static DEC_ZC_QOIS: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_RAW: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_HEVC: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_VP9: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_H263: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_MPEG2V: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_THEORA: AtomicU8 = AtomicU8::new(0);
static DEC_ZC_AVC: AtomicU8 = AtomicU8::new(0);
/// 每格式流式开关（0/1）。
static DEC_STREAM_QOIS: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_RAW: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_HEVC: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_VP9: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_H263: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_MPEG2V: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_THEORA: AtomicU8 = AtomicU8::new(0);
static DEC_STREAM_AVC: AtomicU8 = AtomicU8::new(0);
/// 每格式缓冲下标（0 = 跟随 `tune.window`，1-32 显式覆盖；已钳制 0-32）。
static DEC_BUF_QOIS: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_RAW: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_HEVC: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_VP9: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_H263: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_MPEG2V: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_THEORA: AtomicU32 = AtomicU32::new(0);
static DEC_BUF_AVC: AtomicU32 = AtomicU32::new(0);

/// 开机 `load()` 调用：把 `DecodeTable`（缺表缺键全默认）装进原子存储。
/// `buffers` 钳 0-32（0 = 跟随 `tune.window`）。
pub(crate) fn store_all(d: &DecodeTable) {
    DEC_THREADS_QOIS.store(d.qois.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_RAW.store(d.raw.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_HEVC.store(d.hevc.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_VP9.store(d.vp9.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_H263.store(d.h263.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_MPEG2V.store(d.mpeg2v.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_THEORA.store(d.theora.threads.as_u8(), Ordering::Relaxed);
    DEC_THREADS_AVC.store(d.avc.threads.as_u8(), Ordering::Relaxed);
    DEC_ZC_QOIS.store(d.qois.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_RAW.store(d.raw.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_HEVC.store(d.hevc.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_VP9.store(d.vp9.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_H263.store(d.h263.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_MPEG2V.store(d.mpeg2v.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_THEORA.store(d.theora.zero_copy as u8, Ordering::Relaxed);
    DEC_ZC_AVC.store(d.avc.zero_copy as u8, Ordering::Relaxed);
    DEC_STREAM_QOIS.store(d.qois.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_RAW.store(d.raw.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_HEVC.store(d.hevc.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_VP9.store(d.vp9.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_H263.store(d.h263.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_MPEG2V.store(d.mpeg2v.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_THEORA.store(d.theora.streaming as u8, Ordering::Relaxed);
    DEC_STREAM_AVC.store(d.avc.streaming as u8, Ordering::Relaxed);
    DEC_BUF_QOIS.store(d.qois.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_RAW.store(d.raw.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_HEVC.store(d.hevc.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_VP9.store(d.vp9.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_H263.store(d.h263.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_MPEG2V.store(d.mpeg2v.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_THEORA.store(d.theora.buffers.min(32), Ordering::Relaxed);
    DEC_BUF_AVC.store(d.avc.buffers.min(32), Ordering::Relaxed);
}

/// 是否有状态软解（顺序解码，其余三项 yes；`qois`/`raw` 非状态）。
fn is_stateful_fmt(fmt: MediaFormat) -> bool {
    !matches!(fmt, MediaFormat::Qois | MediaFormat::Raw)
}

fn decode_threads_raw(fmt: MediaFormat) -> DecodeThreads {
    let v = match fmt {
        MediaFormat::Qois => DEC_THREADS_QOIS.load(Ordering::Relaxed),
        MediaFormat::Raw => DEC_THREADS_RAW.load(Ordering::Relaxed),
        MediaFormat::Hevc => DEC_THREADS_HEVC.load(Ordering::Relaxed),
        MediaFormat::Vp9 => DEC_THREADS_VP9.load(Ordering::Relaxed),
        MediaFormat::H263 => DEC_THREADS_H263.load(Ordering::Relaxed),
        MediaFormat::Mpeg2v => DEC_THREADS_MPEG2V.load(Ordering::Relaxed),
        MediaFormat::Theora => DEC_THREADS_THEORA.load(Ordering::Relaxed),
        MediaFormat::Avc => DEC_THREADS_AVC.load(Ordering::Relaxed),
    };
    DecodeThreads::from_u8(v)
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
    let want = decode_threads_raw(fmt);
    if want != DecodeThreads::All {
        return DecodeThreads::BspOnly;
    }
    if is_stateful_fmt(fmt) {
        log::warn!(
            "decode: {} threads all deferred (staying bsp_only)",
            decode_key(fmt)
        );
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
    let v = match fmt {
        MediaFormat::Qois => DEC_ZC_QOIS.load(Ordering::Relaxed),
        MediaFormat::Raw => DEC_ZC_RAW.load(Ordering::Relaxed),
        MediaFormat::Hevc => DEC_ZC_HEVC.load(Ordering::Relaxed),
        MediaFormat::Vp9 => DEC_ZC_VP9.load(Ordering::Relaxed),
        MediaFormat::H263 => DEC_ZC_H263.load(Ordering::Relaxed),
        MediaFormat::Mpeg2v => DEC_ZC_MPEG2V.load(Ordering::Relaxed),
        MediaFormat::Theora => DEC_ZC_THEORA.load(Ordering::Relaxed),
        MediaFormat::Avc => DEC_ZC_AVC.load(Ordering::Relaxed),
    };
    v != 0
}

/// 解码流式开关（`[decode.<fmt>] streaming`，默认 false）。
pub fn decode_streaming(fmt: MediaFormat) -> bool {
    let v = match fmt {
        MediaFormat::Qois => DEC_STREAM_QOIS.load(Ordering::Relaxed),
        MediaFormat::Raw => DEC_STREAM_RAW.load(Ordering::Relaxed),
        MediaFormat::Hevc => DEC_STREAM_HEVC.load(Ordering::Relaxed),
        MediaFormat::Vp9 => DEC_STREAM_VP9.load(Ordering::Relaxed),
        MediaFormat::H263 => DEC_STREAM_H263.load(Ordering::Relaxed),
        MediaFormat::Mpeg2v => DEC_STREAM_MPEG2V.load(Ordering::Relaxed),
        MediaFormat::Theora => DEC_STREAM_THEORA.load(Ordering::Relaxed),
        MediaFormat::Avc => DEC_STREAM_AVC.load(Ordering::Relaxed),
    };
    v != 0
}

/// 每格式解码窗下标（`[decode.<fmt>] buffers` 原始值 0-32：0 跟随全局，
/// 1-32 显式覆盖；已钳制）。
pub fn decode_buffers(fmt: MediaFormat) -> u32 {
    let v = match fmt {
        MediaFormat::Qois => DEC_BUF_QOIS.load(Ordering::Relaxed),
        MediaFormat::Raw => DEC_BUF_RAW.load(Ordering::Relaxed),
        MediaFormat::Hevc => DEC_BUF_HEVC.load(Ordering::Relaxed),
        MediaFormat::Vp9 => DEC_BUF_VP9.load(Ordering::Relaxed),
        MediaFormat::H263 => DEC_BUF_H263.load(Ordering::Relaxed),
        MediaFormat::Mpeg2v => DEC_BUF_MPEG2V.load(Ordering::Relaxed),
        MediaFormat::Theora => DEC_BUF_THEORA.load(Ordering::Relaxed),
        MediaFormat::Avc => DEC_BUF_AVC.load(Ordering::Relaxed),
    };
    v.min(32)
}

/// 每格式生效解码窗（帧数）：`buffers = 0` 继承全局 `tune.window`（1-32），
/// 显式 1-32 按格式覆盖；`tune.window` 仍是全局默认。
pub fn decode_window(fmt: MediaFormat) -> usize {
    match decode_buffers(fmt) {
        0 => super::tune_window().clamp(1, 32).max(1) as usize,
        n => n.clamp(1, 32).max(1) as usize,
    }
}
