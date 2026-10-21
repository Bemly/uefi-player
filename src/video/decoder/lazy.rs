//! 懒供片（`[play] feed` 三档：staged/stream/zero_copy，`clip` 不进此文件）。
//!
//! - load 期只分块（纯扫描，不解码像素）+ 首帧预解（定几何、空集报错，
//!   boot 语义与全预解码一致：坏片源直接 Err，不断 boot）；
//! - 编码块：qois 按 `u32le` 帧长；avc/hevc 按 Annex-B NALU + AU 组
//!   （`demux::split_annexb_aus`，Agent-B 未合入前为整流单 AU stub）；
//!   vp9 按 IVF chunk/mp4 样片；h263 流式 reader 免预切；mpeg2v 按
//!   `access_units`；theora 按 Ogg 包；raw 按帧文件；mp4 先整文件 demux 出
//!   annexb/样片再分块（demux 本来就要整文件读，内存赢在解码帧侧）；
//! - 三策略：staged（qois 走 QOI→Raw→Blt 显式三段，复用 `buffer.rs` 三缓冲；
//!   非 qois 在结构上等价于 clip——软解码 planar→BGRA 固有分段，
//!   故 `feed_for` 直接裁决回全预解码，不进此文件）、stream（`tune.window`
//!   窗口边播边解，游标之后淘汰；`rewind` 重建解码器状态重播）、zero_copy
//!   （单复用帧缓冲逐显示帧即时解码；借用契约：调用方须立即上传，
//!   `blt/direct_loop` 与 `nano_pump` 满足）；
//! - 几何在首帧后冻结（窗口缓冲按此尺寸复用；与全预解码的"混入异形帧跳过"
//!   同策略：后继异形显示帧记数跳过；theora 全预解码本无一致性检查，
//!   此处为保窗口复用加检查，见 `docs/player.md`）；
//! - 有状态编解码器（avc/hevc/vp9/h263/mpeg2v/theora）顺序喂块，状态机常驻
//!   `Source`；`rewind` 丢弃增量上下文重建（qois/raw 无状态，只归零下标）；
//! - 坏块记数跳过（与全预解码同策略）；avc 逐 AU 经 `decode`（解码序，
//!   全预解码的 `decode_stream` 是显示序，B 帧/多 slice 流以 Agent-B 联调结论为准）；
//! - 上传路零改：只经 `next`/`reset`/`drain_all` 与 `Clip` 的窗口交互。

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use uefi::proto::console::gop::BltPixel;

use super::source::Source;
use crate::config::{FeedMode, tune_window};
use crate::video::buffer::{BltFrameBuffer, QoiFrameBuffer, RawFrameBuffer};
use crate::video::demux::split_annexb_aus;

/// 供片裁决（各 `from_*` 包装共用）：返回 `Some` 即走懒解码。
/// - `clip` 全量预解码（默认，零行为变化）；
/// - 壁纸恒全量预解码（随机访问循环：idle 菜单背景/引导过渡按索引取帧；
///   `wallpaper.rs` 零改，故以 `tag` 裁决，矩阵规则见 `docs/player.md`）；
/// - `staged` 非 qois 在结构上等价于全预解码（软解码 planar→BGRA 固有分段，
///   无 QOI→Raw→Blt 三段可走），直接回全预解码；
/// - `staged_ok` 由各格式包装传入（只有 qois 为 true）。
pub(crate) fn feed_for(tag: &str, staged_ok: bool) -> Option<FeedMode> {
    let m = crate::config::play_feed();
    if m == FeedMode::Clip {
        return None;
    }
    if tag == "wallpaper" {
        return None;
    }
    if m == FeedMode::Staged && !staged_ok {
        return None;
    }
    Some(m)
}

/// 懒策略（`FeedMode` 去掉 `Clip` 后的三档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Staged,
    Stream,
    ZeroCopy,
}

impl Strategy {
    fn of(mode: FeedMode) -> Self {
        match mode {
            FeedMode::Staged => Self::Staged,
            FeedMode::ZeroCopy => Self::ZeroCopy,
            FeedMode::Stream | FeedMode::Clip => Self::Stream,
        }
    }
}

/// 懒供片状态（`Clip.lazy` 的内容）：编码分块保留 + 解码会话 + 策略窗口。
pub(crate) struct LazyState {
    source: Source,
    strategy: Strategy,
    /// stream 保留的游标后帧数（`tune.window`）；staged/zero_copy 恒 1。
    window: usize,
    /// `frames[0]` 的绝对显示序号（stream 淘汰前缀时前移；staged/zc 恒等于 cursor）。
    base: usize,
    skipped: u32,
    /// 已产出显示帧数（串口留数用）。
    produced: u32,
    finished: bool,
    end_warned: bool,
    /// 复用解码目标（staged/zc 与窗口共享，稳态零分配；stream 淘汰帧回收于此）。
    scratch: Vec<BltPixel>,
    /// staged qois 显式三段缓冲（`buffer.rs`，其余策略空置）。
    qoi: QoiFrameBuffer,
    raw: RawFrameBuffer,
    blt: BltFrameBuffer,
}

fn fail(tag: &str, skipped: u32) -> crate::error::NyaStatus {
    crate::error::NyaStatus::_Debug(alloc::format!(
        "{}: no valid frames ({} skipped)",
        tag,
        skipped
    ))
}

impl LazyState {
    /// 构造（load 期）：分块已备，会话初建；首帧预解定几何（空集报错，
    /// 与全预解码同消息形状），随后重建会话保证从头重播语义。
    fn build(
        tag: &str,
        mode: FeedMode,
        mut source: Source,
        total: usize,
        preset_geom: Option<(usize, usize)>,
    ) -> crate::error::Result<super::Clip> {
        let strategy = Strategy::of(mode);
        let window = match strategy {
            Strategy::Stream => tune_window() as usize,
            Strategy::Staged | Strategy::ZeroCopy => 1,
        };
        // 首帧预解：定几何 + 空集报错（boot 语义与全预解码一致）。
        let mut geom = preset_geom;
        let mut first: Vec<BltPixel> = Vec::new();
        let mut skipped = 0u32;
        // staged 走显式三段探（与回放同路）。
        let mut qoi = QoiFrameBuffer::new(0);
        let mut raw = RawFrameBuffer::new(0);
        let mut blt = BltFrameBuffer::new(0);
        let probed = if strategy == Strategy::Staged {
            qoi = QoiFrameBuffer::new(8 * 1024 * 1024);
            raw = RawFrameBuffer::new(8 * 1024 * 1024);
            if source.next_staged(&mut qoi, &mut raw, &mut blt, &mut geom, &mut skipped) {
                first.extend_from_slice(&blt.0);
                true
            } else {
                false
            }
        } else {
            source.next_into(&mut first, &mut geom, &mut skipped)
        };
        if !probed {
            return Err(fail(tag, skipped));
        }
        let (w, h) = geom.unwrap_or((0, 0));
        if skipped > 0 {
            log::warn!("{}: predecoded 1 frames, skipped {} bad", tag, skipped);
        }
        // 重建会话（有状态编解码器丢弃预解的增量上下文，从首分块重播）。
        source.reset();
        if strategy == Strategy::Staged {
            // staged 缓冲按定几何配（Blt 槽恰帧大小；QOI/Raw 仍 8M，与旧循环同量级）。
            blt = BltFrameBuffer::new(w.saturating_mul(h));
        }
        let frames = alloc::vec![first];
        Ok(super::Clip {
            w,
            h,
            frames,
            cursor: 0,
            lazy: Some(LazyState {
                source,
                strategy,
                window: window.max(1),
                base: 0,
                skipped,
                produced: 1,
                finished: false,
                end_warned: false,
                scratch: Vec::new(),
                qoi,
                raw,
                blt,
            }),
            total,
        })
    }

    pub(crate) fn base(&self) -> usize {
        self.base
    }

    /// 填充一帧进 `scratch`（staged 经三段进 `blt` 再倒手，保证复用）。
    fn fill(&mut self, geom: &mut Option<(usize, usize)>) -> bool {
        if self.strategy == Strategy::Staged {
            let (qoi, raw, blt) = (&mut self.qoi, &mut self.raw, &mut self.blt);
            if !self
                .source
                .next_staged(qoi, raw, blt, geom, &mut self.skipped)
            {
                return false;
            }
            core::mem::swap(&mut blt.0, &mut self.scratch);
            self.produced += 1;
            true
        } else if self
            .source
            .next_into(&mut self.scratch, geom, &mut self.skipped)
        {
            self.produced += 1;
            true
        } else {
            false
        }
    }

    /// 逐显示帧供片（`Clip::next_frame` 唯一落点）：命中窗口直接借，
    /// 未命中按策略填充；流尽返回 `None`（调用方 `rewind` 循环）。
    /// 跳帧（`pace_or_drop`）同样经此解码丢弃：正确但费解码，见 `docs/player.md`。
    pub(crate) fn next<'a>(
        &mut self,
        frames: &'a mut Vec<Vec<BltPixel>>,
        cursor: &mut usize,
        w: usize,
        h: usize,
    ) -> Option<&'a [BltPixel]> {
        let mut geom = Some((w, h));
        // 填充到覆盖游标（staged/zc 单槽：未命中即解一块顶掉旧槽）。
        while !self.finished && *cursor >= self.base + frames.len() {
            if self.strategy == Strategy::Stream {
                if !self.fill(&mut geom) {
                    self.finished = true;
                    break;
                }
                frames.push(core::mem::take(&mut self.scratch));
            } else {
                if !self.fill(&mut geom) {
                    self.finished = true;
                    break;
                }
                if frames.is_empty() {
                    frames.push(Vec::new());
                }
                core::mem::swap(&mut frames[0], &mut self.scratch);
                self.base = *cursor;
            }
        }
        // stream 游标后淘汰（淘汰帧回收进 scratch 复用；未 serving 帧永不动）。
        if self.strategy == Strategy::Stream {
            while self.base < cursor.saturating_sub(self.window)
                && self.base + frames.len() > *cursor
            {
                self.scratch = frames.remove(0);
                self.base += 1;
            }
        }
        if *cursor >= self.base && *cursor - self.base < frames.len() {
            let frame = &frames[*cursor - self.base];
            *cursor += 1;
            self.note_end();
            Some(frame)
        } else {
            self.note_end();
            None
        }
    }

    /// 流尽记一笔（串口留数，首轮一次；`rewind` 后复位）。
    fn note_end(&mut self) {
        if self.finished && !self.end_warned {
            self.end_warned = true;
            if self.skipped > 0 {
                log::warn!(
                    "video: streamed {} frames, skipped {} bad",
                    self.produced,
                    self.skipped
                );
            }
        }
    }

    /// 重播（`Clip::rewind` 唯一落点）：重建解码器状态，游标归零；
    /// 窗口清空（首槽回收进 scratch 复用），下一 `next` 从首块重解。
    pub(crate) fn reset(&mut self, frames: &mut Vec<Vec<BltPixel>>, cursor: &mut usize) {
        self.source.reset();
        if let Some(first) = frames.first_mut() {
            core::mem::swap(first, &mut self.scratch);
        }
        frames.clear();
        *cursor = 0;
        self.base = 0;
        self.finished = false;
        self.skipped = 0;
        self.produced = 0;
        self.end_warned = false;
    }

    /// 强制全量（`Clip::ensure_eager` 唯一落点）：停淘汰，余块全解进 `frames`。
    /// 前提 load 后即调（cursor==0，`frames=[F0]`）；失败留半集转全量，
    /// 调用方按加载失败处理（与构造期报错同 UX）。
    pub(crate) fn drain_all(
        &mut self,
        frames: &mut Vec<Vec<BltPixel>>,
        w: usize,
        h: usize,
    ) -> crate::error::Result {
        let mut geom = Some((w, h));
        self.window = usize::MAX;
        while !self.finished {
            if !self.fill(&mut geom) {
                self.finished = true;
                break;
            }
            frames.push(core::mem::take(&mut self.scratch));
        }
        // staged 经 `fill` 倒手后帧在 `scratch`，与 stream 同式 push，无残留特例。
        if self.skipped > 0 {
            log::warn!(
                "video: ensured {} frames, skipped {} bad",
                frames.len(),
                self.skipped
            );
        }
        Ok(())
    }
}

/// qois 纯扫描分块（`u32le` 帧长；截断即止，与全预解码遍历同条件）。
fn scan_qois(data: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 4 <= data.len() {
        let n =
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let start = off + 4;
        let end = start.saturating_add(n);
        if end > data.len() {
            break;
        }
        out.push(start..end);
        off = end;
    }
    out
}

/// IVF 纯扫描分块（32 字节文件头 + 12 字节帧头 + 负载；截断即止）。
fn scan_ivf(data: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if data.len() < 32 {
        return out;
    }
    let mut off = 32usize;
    while off + 12 <= data.len() {
        let sz =
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let start = off + 12;
        let end = start.saturating_add(sz);
        if end > data.len() {
            break;
        }
        out.push(start..end);
        off = end;
    }
    out
}

/// avc VCL 类型集（1 字节头 `nal_type` 1..=5，含 IDR；供 `split_annexb_aus`）。
const AVC_VCL: &[u8] = &[1, 2, 3, 4, 5];
/// hevc VCL 类型集（2 字节头 `(b[0] >> 1) & 0x3f`：0..=9 走带 + 16..=21 IRAP）。
const HEVC_VCL: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17, 18, 19, 20, 21];

/// 以下各 `*_bytes` 即各 `from_*` 的懒分支（签名见 `super::Clip` 包装）：
/// 分块（纯扫描）→ `LazyState::build`（首帧预解 + 会话重建）。
pub(crate) fn qois_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    log::warn!("{}: lazy qois enter len={}", tag, data.len());
    let ranges = scan_qois(data);
    log::warn!("{}: lazy qois chunks={}", tag, ranges.len());
    let total = ranges.len();
    let source = Source::Qois {
        data: data.to_vec(),
        ranges,
        pos: 0,
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn hevc_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    let data = data.to_vec();
    let aus = split_annexb_aus(&data, HEVC_VCL, 2);
    let total = aus.len().max(1);
    let source = Source::Hevc {
        data,
        aus,
        pos: 0,
        dec: rusty_h265::Decoder::new(),
        pending: VecDeque::new(),
        flushed: false,
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn avc_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    let data = data.to_vec();
    let aus = split_annexb_aus(&data, AVC_VCL, 1);
    let total = aus.len().max(1);
    let source = Source::Avc {
        data,
        aus,
        pos: 0,
        dec: rusty_h264_decoder::Decoder::new(),
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn vp9_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    let ranges = scan_ivf(data);
    let total = ranges.len();
    let source = Source::Vp9Ivf {
        data: data.to_vec(),
        ranges,
        pos: 0,
        dec: vp9dec::Decoder::new(),
        pending: VecDeque::new(),
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn vp9_raw_frames(
    samples: &[Vec<u8>],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    let total = samples.len();
    let source = Source::Vp9Mp4 {
        frames: samples.to_vec(),
        pos: 0,
        dec: vp9dec::Decoder::new(),
        pending: VecDeque::new(),
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn h263_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    use h263_rs::parser::H263Reader;
    let leaked: &'static mut [u8] = alloc::boxed::Box::leak(data.to_vec().into_boxed_slice());
    let s: &'static [u8] = leaked;
    let source = Source::H263 {
        data: s,
        reader: H263Reader::from_source(s),
        state: h263_rs::H263State::new(h263_rs::DecoderOption::empty()),
    };
    // 流式免预切：总数未知（`None` 截断，`len` 取 `MAX`）。
    LazyState::build(tag, mode, source, usize::MAX, None)
}

pub(crate) fn mpeg2v_bytes(
    data: &[u8],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    let data = data.to_vec();
    let aus = filmcraft_mpeg2v::access_units(&data);
    let total = aus.len();
    let source = Source::Mpeg2v {
        data,
        aus,
        pos: 0,
        dec: filmcraft_mpeg2v::Decoder::new(),
        pending: VecDeque::new(),
        flushed: false,
        pts: 0,
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn theora_packets(
    packets: &[Vec<u8>],
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    if packets.len() < 3 {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: theora needs 3 header packets, got {}",
            tag,
            packets.len()
        )));
    }
    // 三头先验（与全预解码同口径：ident 420 + comment 可解析 + setup），
    // 会话即建（`reset` 时重解，见 `Source::reset`）。
    let ident = oxideav_theora::decode_identification_header(&packets[0]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora ident error: {:?}", tag, e))
    })?;
    if ident.pf != oxideav_theora::PixelFormat::Yuv420 {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: theora only supports 4:2:0 (got {:?})",
            tag,
            ident.pf
        )));
    }
    let _ = oxideav_theora::parse_comment_header(&packets[1]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora comment error: {:?}", tag, e))
    })?;
    let setup = oxideav_theora::decode_setup_header(&packets[2]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora setup error: {:?}", tag, e))
    })?;
    let dec = oxideav_theora::FrameDecoder::new(ident, setup).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora decoder error: {:?}", tag, e))
    })?;
    let total = packets.len().saturating_sub(3);
    let source = Source::Theora {
        packets: packets.to_vec(),
        pos: 3,
        dec,
    };
    LazyState::build(tag, mode, source, total, None)
}

pub(crate) fn raw_dir(
    fs: &mut crate::fs::Fs,
    dir: &str,
    want: usize,
    frame_bytes: usize,
    tag: &str,
    mode: FeedMode,
) -> crate::error::Result<super::Clip> {
    // 目录枚举与全预解码同口径（缺目录/空目录直接 Err，boot 语义一致）。
    let cdir =
        uefi::CString16::try_from(dir).map_err(|_| crate::error::NyaStatus::FromStrWithBufError)?;
    let entries = match fs.list_dir(&cdir) {
        Ok(e) => e,
        Err(crate::error::NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            return Err(crate::error::NyaStatus::_Debug(alloc::format!(
                "{}: no {} directory",
                tag,
                dir
            )));
        }
        Err(e) => {
            log::warn!("{}: list failed: {:?}", tag, e);
            return Err(e);
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
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no frames in {}",
            tag,
            dir
        )));
    }
    let names: Vec<String> = names.into_iter().take(want.max(1)).collect();
    let total = names.len();
    let source = Source::Raw {
        dir: String::from(dir),
        names,
        pos: 0,
        frame_bytes,
    };
    // raw 几何定值（1920x1080，全预解码同值；首帧预解只验可读性）。
    LazyState::build(tag, mode, source, total, Some((1920, 1080)))
}
