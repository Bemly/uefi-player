//! 统一解码控制面（`[play] feed` 5 档 + `[decode.<fmt>]` 每格式表）：
//! `feed`（clip/staged/stream/zero_copy/single）切拿帧方式，`decode` 表
//! （threads/zero_copy/streaming/buffers）调每格式解码行为。BSP 解码、AP 只
//! 拷屏（永不碰 GOP/Blt）；解码 SIMD 单比特跟随全局（`config::resolve_accel`
//! 一推，见该函数尾）；`tune.window` 仍是全局默认，`buffers=0` 跟随它。
//!
//! 每格式可行性（单核现状，`Source` 状态机顺序；`threads=all` 在有状态软
//! 解上解析但告警仍 `bsp_only`，`qois`/`raw` 立即兑现帧级并行）：
//! qois 无状态逐帧→帧级并行可行，零拷/流/多缓冲全 yes；raw 零解码→全 yes；
//! hevc 有状态 Annex-B+输出队列/重排→解码顺序（demux→解码→转换交叠 deferred），
//! 零拷/流/窗缓冲 yes；avc 显示序 vs 解码序+B 帧/多 slice 待 Agent-B 联调→顺序，
//! 其余 yes；vp9 有状态+隐藏 altref 跳过→顺序，其余 yes；h263 流式 reader 有
//! 状态、总量未知→顺序，其余 yes；mpeg2v AU 切分有状态仅 4:2:0→顺序，其余 yes；
//! theora Ogg 包+三头状态→顺序，其余 yes。
//!
//! 本文件垫片直调 `config::decode_*` 原子存储（单源；`fetch`/`effective_feed` 同式），
//! 矩阵靠串口 `decode:`/`video: feed=` 行断言，画质不断言）。

use alloc::vec;
use alloc::vec::Vec;
use uefi::Status;
use uefi::proto::console::gop::BltPixel;
use uefi::proto::media::file::{File, FileInfo, RegularFile};

use super::demux::Mp4VideoKind;
use crate::config::{DecodeThreads, FeedMode, MediaFormat};

mod eager;
mod lazy;
mod source;

use lazy::LazyState;

/// slots 路转调（`mp_slots` 用；只转调，零逻辑）：
/// `lazy::scan_qois_chunks`（BSP scan-split 复用）+
/// `lazy::list_raw_names`（raw 名表枚举复用）+
/// `lazy::decode_qois_chunk`/`lazy::decode_raw_frame`（AP 按序号解单块）。
pub(crate) use lazy::{decode_qois_chunk, decode_raw_frame, list_raw_names, scan_qois_chunks};

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

/// 每格式解码垫片（直调 `config::decode_*` 原子存储；`Config` 无旧二次解析，
/// 缺表缺键全默认不断整文件；矩阵靠 `decode:`/`video: feed=` 行断言）。
/// 有状态 `all` 解析但 warn 仍 `bsp_only`；`qois`/`raw` 立即兑现（串口串与 config 侧逐字一致）。
pub fn decode_threads(fmt: MediaFormat) -> DecodeThreads {
    crate::config::decode_threads(fmt)
}

/// 垫片：`config::decode_zero_copy(fmt) -> bool`（显式开关，缺省 false；
/// `feed=zero_copy` 照常走零拷复用，本项为叠加 hint，稳态零分配见 `lazy`）。
pub fn decode_zero_copy(fmt: MediaFormat) -> bool {
    crate::config::decode_zero_copy(fmt)
}

/// 垫片：`config::decode_streaming(fmt) -> bool`（显式开关，缺省 false；
/// `feed=stream` 照常窗口边播边解；`raw` 逐文件 + `h263` reader 恒真流式）。
pub fn decode_streaming(fmt: MediaFormat) -> bool {
    crate::config::decode_streaming(fmt)
}

/// 垫片：`config::decode_buffers(fmt) -> u32`（raw 值 0-32：0 跟随
/// `tune.window`，1-32 显式覆盖；钳制 0-32，矩阵靠 `decode:` 行断言）。
pub fn decode_buffers(fmt: MediaFormat) -> u32 {
    crate::config::decode_buffers(fmt)
}

/// 每格式解码窗（`buffers=0` 继承全局 `tune.window` 1-32，显式 1-32 覆盖）。
pub(crate) fn decode_window(fmt: MediaFormat) -> usize {
    crate::config::decode_window(fmt)
}

/// 垫片：`config::decode_fetch(fmt)`（每格式供片覆盖，None = 继承全局）。
pub fn decode_fetch(fmt: MediaFormat) -> Option<FeedMode> {
    crate::config::decode_fetch(fmt)
}

/// 有效供片：每格式 `fetch.unwrap_or(全局 feed)`（解码包装裁决用）。
/// 壁纸恒 clip 由 `lazy::feed_for` 以 tag 裁决，此处不分支。
pub(crate) fn effective_feed(fmt: MediaFormat) -> FeedMode {
    crate::config::effective_feed(fmt)
}

/// `effective_feed` 别名（供旧名调用，同值）。
pub(crate) fn decode_effective_feed(fmt: MediaFormat) -> FeedMode {
    crate::config::decode_effective_feed(fmt)
}

/// `feed="single"` 识别（全局旧名，壁纸外全局 single 即真；每格式覆盖见 `feed_is_single`；
/// 壁纸恒 `clip`，见 `lazy::feed_for`）。
pub(crate) fn feed_is_single_raw() -> bool {
    crate::config::play_feed() == FeedMode::Single
}

/// 每格式 single 识别：有效供片 == single 且非壁纸 tag（壁纸恒 clip 忽略覆盖）。
pub(crate) fn feed_is_single(fmt: MediaFormat, tag: &str) -> bool {
    tag != "wallpaper" && effective_feed(fmt) == FeedMode::Single
}

/// 单行解码面日志（矩阵 `parse` 断言用，不断言画质）：每 `Clip` 构造记一笔。
pub(crate) fn log_decode_plane(fmt: MediaFormat, tag: &str) {
    log::info!(
        "decode: tag={} fmt={} threads={} zero_copy={} streaming={} buffers={} window={}",
        tag,
        decode_key(fmt),
        decode_threads(fmt).as_str(),
        decode_zero_copy(fmt),
        decode_streaming(fmt),
        decode_buffers(fmt),
        decode_window(fmt),
    );
}

/// raw 目录列举形（media 裸名/根帧落点）：`""`（媒体根）列目录用 `\`（根枚举形）。
pub(crate) fn raw_list_dir(dir: &str) -> &str {
    if dir.is_empty() { "\\" } else { dir }
}

/// raw 文件拼接形（同上落点）：根拼 `\name` 根绝对形，避免 `\\name` 双分隔；
/// 普通子目录沿旧 `{dir}\{name}` 相对形。
pub(crate) fn raw_file_path(dir: &str, name: &str) -> alloc::string::String {
    if dir.is_empty() || dir == "\\" {
        alloc::format!("\\{}", name)
    } else {
        alloc::format!("{}\\{}", dir, name)
    }
}

/// 全量预解码内存预算（eager 共收口，八格式共用）：256MB。
/// 口径：1080p BGRA 单帧 1920*1080*4 ≈ 8MB，256MB ≈ 30 帧；
/// 长片 40s@60fps 全预解码要 ~19GB（见 `docs/transcode.md` 内存纪律，与 handover 同源），
/// 固件侧不可能常驻，故超预算直接 Err（帧太多切 stream 供片），绝不 panic/OOM。
/// 只卡全量预分配（clip/壁纸/mp 强制 eager + `ensure_eager` 全量兜底）；
/// stream/zero_copy/single 懒路径（窗口/单槽/零缓存）不受影响。
pub(crate) const CLIP_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// 逐帧累加预算检查（eager 全量分配唯一收口逻辑）：`frames` 为将达到的帧数，
/// `w*h*4` 为单帧 BGRA 字节；超预算即 warn + Err（`NyaStatus::_Debug` feeder 口径，
/// 文案含“too many frames for clip, use stream”+“帧太多切 stream 供片”，
/// 外层 `playlist: skip` / `ptui: play failed` 原样透出，不断播）。
pub(crate) fn check_clip_budget(
    frames: usize,
    w: usize,
    h: usize,
    tag: &str,
) -> crate::error::Result<()> {
    let per = w.saturating_mul(h).saturating_mul(4);
    let need = (frames as u64).saturating_mul(per as u64);
    if need > CLIP_BUDGET_BYTES as u64 {
        log::warn!(
            "{}: too many frames for clip, use stream (帧太多切 stream 供片, {} frames {}x{} > 256MB budget)",
            tag,
            frames,
            w,
            h
        );
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: too many frames for clip, use stream (帧太多切 stream 供片, {} frames {}x{} > 256MB budget)",
            tag,
            frames,
            w,
            h
        )));
    }
    Ok(())
}

//////// 统一播放剪辑：预解码帧集（`Vec<Vec<BltPixel>>` + 游标），`[play] feed` 五档供片。
//////// 对外 API 冻结（`video.rs`/`menu.rs`/`wallpaper.rs`/`exec.rs` 零改）：
//////// 字段 `w,h,frames,cursor` + 方法 `next_frame/rewind/len/frame` 签名不变；
//////// 内部按 `feed` 切供片方式（见 `lazy.rs`）：`clip` 全量预解码（默认，
//////// 与旧行为逐字节一致）/`staged`（qois 显式三段，其余等价 clip）/
//////// `stream`（每格式窗边播边解，`decode_buffers` 显式或跟随 `tune.window`）/
//////// `zero_copy`（单复用缓冲即时解码）/`single`（零缓存逐帧重解，`render.blt` 同旧环）。
//////// 供片源八格式（`[play]/[wallpaper] format` 矩阵，见 `config::MediaFormat`）：
//////// - qois：`u32le len + QOI` 串接文件，解码为 BGRA（播放器 P3 老路）；
//////// - raw：`frameNNNN.raw` BGRA 直存序列，零解码直搬（壁纸 P4 老路）；
//////// - hevc：`video.mp4`（hev 轨，石雨堂拆包 → Annex-B）或 `video.hevc`
////////   （Annex-B 裸流）→ vendor `rusty_h265` 软解 → YUV420 转 BGRA；
//////// - avc：`video.mp4`（avc 轨，石雨堂拆 avcC → Annex-B）→ vendor
////////   `rusty_h264_decoder` 软解 → YUV420 转 BGRA；
//////// - vp9：`video.mp4`（vp09 轨，样片即裸帧）或 `video.ivf`（IVF 容器）
////////   → vendor `vp9dec` 软解 → YUV420 转 BGRA；
//////// - h263：`video.h263` 裸流 → vendor `h263-rs` 软解 → YUV420 转 BGRA；
//////// - mpeg2v：`video.m2v` 裸基本流（接入单元切分）→ vendor
////////   `filmcraft-mpeg2v` 软解 → YUV420 转 BGRA（只收 4:2:0）；
//////// - theora：`video.ogv`（OggS 磁盘原格式，手写拆包）→ vendor
////////   `oxideav-theora` 软解 → 显示区裁剪 + 翻转 + YUV420 转 BGRA（只收 4:2:0）。
//////// 壁纸只是薄调：`wallpaper::load` 按开关选格式调这里（`wall.*` 同名文件），
//////// 循环/上屏走同一套（壁纸恒预解码：随机访问循环，以 `tag` 裁决，见 `lazy::feed_for`）。
pub struct Clip {
    /// 帧几何（qois 取文件头一致值，raw 取首帧/定值；上屏方按此切行）。
    pub w: usize,
    pub h: usize,
    /// 预解码帧（BGRA `BltPixel`，与显存布局一致）。
    /// 懒模式下是解码窗口（stream 保留 `window` 帧，staged/zero_copy 只留 1 帧），
    /// 直接遍历它只在全预解码（clip/壁纸/mp 强制）下有意义，见 `len`/`frame`。
    pub frames: Vec<Vec<BltPixel>>,
    pub cursor: usize,
    /// 懒供片状态（`None` = 全量预解码，与旧行为一致）。
    pub(crate) lazy: Option<LazyState>,
    /// 显示帧总数：eager 下恒等于 `frames.len()`；懒模式下是 load 期纯扫描的分块
    /// 上界（坏块跳过后偏大，`None` 截断；h263 流式免预切，总数未知取 `usize::MAX`）。
    pub(crate) total: usize,
}

impl Clip {
    /// 全预解码构造（`eager.rs` 落点共用，八格式唯一收口）：`lazy` 为空，`total` 即帧数。
    /// 预算即 `check_clip_budget`（256MB，1080p ≈ 30 帧，与 handover 19GB/40s@60fps 同源）；
    /// 超预算直接 Err（帧太多切 stream 供片），绝不 panic；懒路径不进此函数。
    pub(crate) fn eager(
        w: usize,
        h: usize,
        frames: Vec<Vec<BltPixel>>,
        tag: &str,
    ) -> crate::error::Result<Self> {
        check_clip_budget(frames.len(), w, h, tag)?;
        let total = frames.len();
        Ok(Self {
            w,
            h,
            frames,
            cursor: 0,
            lazy: None,
            total,
        })
    }

    /// qois 字节流解码（`tag` 只进串口：`video` / `wallpaper`）。
    /// 供片裁决见 `lazy::feed_for`（clip 全预解码 / 壁纸恒预解码 / 其余懒解码；有效供片 = 每格式 fetch 覆盖全局）。
    /// `single` 先行（有效供片识别，壁纸除外）；解码面单行日志供矩阵 parse 断言。
    pub fn from_qois_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Qois, tag);
        if feed_is_single(MediaFormat::Qois, tag) {
            return lazy::qois_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Qois, true) {
            return lazy::qois_bytes(data, tag, mode);
        }
        eager::from_qois_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.qois` / `wall.qois`）。
    pub fn from_qois_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_qois_bytes(&buf, tag)
    }

    /// hevc 字节流解码（Annex-B，`tag` 只进串口）。
    pub fn from_hevc_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Hevc, tag);
        if feed_is_single(MediaFormat::Hevc, tag) {
            return lazy::hevc_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Hevc, false) {
            return lazy::hevc_bytes(data, tag, mode);
        }
        eager::from_hevc_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.hevc`，Annex-B 带内 VPS/SPS/PPS）。
    pub fn from_hevc_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_hevc_bytes(&buf, tag)
    }

    /// vp9 字节流解码（IVF 容器，`tag` 只进串口）。
    pub fn from_vp9_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Vp9, tag);
        if feed_is_single(MediaFormat::Vp9, tag) {
            return lazy::vp9_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Vp9, false) {
            return lazy::vp9_bytes(data, tag, mode);
        }
        eager::from_vp9_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.ivf`，32 字节头 + 12 字节帧头 + 负载）。
    pub fn from_vp9_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_vp9_bytes(&buf, tag)
    }

    /// MP4 容器解码（`tag` 只进串口）：石雨堂拆轨 → avc/hevc 拼 Annex-B，
    /// vp9 裸帧逐样片。demux 本来就要整文件读（内存赢在解码帧侧），
    /// 故拆包在分支前共用，懒模式只把 annexb/样片转成分块保留。
    pub fn from_mp4_file(
        file: RegularFile,
        want: Mp4VideoKind,
        tag: &str,
    ) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        let track = super::demux::demux_mp4_track(&buf, want, tag)?;
        let _ = buf;
        // 拆包共用，分派走各字节流包装（供片裁决在内，见 `feed_for`）。
        match want {
            Mp4VideoKind::Avc => Self::from_avc_bytes(&track.annexb, tag),
            Mp4VideoKind::Hevc => Self::from_hevc_bytes(&track.annexb, tag),
            Mp4VideoKind::Vp9 => Self::from_vp9_raw_frames(&track.raw_frames, tag),
        }
    }

    /// vp9 裸帧序列解码（MP4 `Vp09` 轨用）。
    pub fn from_vp9_raw_frames(samples: &[Vec<u8>], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Vp9, tag);
        if feed_is_single(MediaFormat::Vp9, tag) {
            return lazy::vp9_single_raw_frames(samples, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Vp9, false) {
            return lazy::vp9_raw_frames(samples, tag, mode);
        }
        eager::from_vp9_raw_frames(samples, tag)
    }

    /// avc 字节流解码（Annex-B，`tag` 只进串口；MP4-only，无裸流路）。
    pub fn from_avc_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Avc, tag);
        if feed_is_single(MediaFormat::Avc, tag) {
            return lazy::avc_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Avc, false) {
            return lazy::avc_bytes(data, tag, mode);
        }
        eager::from_avc_bytes(data, tag)
    }

    /// h263 裸流解码（`tag` 只进串口；流式 reader，懒模式免预切）。
    pub fn from_h263_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::H263, tag);
        if feed_is_single(MediaFormat::H263, tag) {
            return lazy::h263_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::H263, false) {
            return lazy::h263_bytes(data, tag, mode);
        }
        eager::from_h263_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.h263`，裸流）。
    pub fn from_h263_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_h263_bytes(&buf, tag)
    }

    /// mpeg2 裸流解码（`tag` 只进串口；接入单元切分，只收 4:2:0）。
    pub fn from_mpeg2v_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Mpeg2v, tag);
        if feed_is_single(MediaFormat::Mpeg2v, tag) {
            return lazy::mpeg2v_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Mpeg2v, false) {
            return lazy::mpeg2v_bytes(data, tag, mode);
        }
        eager::from_mpeg2v_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.m2v`，裸基本流）。
    pub fn from_mpeg2v_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_mpeg2v_bytes(&buf, tag)
    }

    /// theora 包序列解码（`tag` 只进串口；ident/comment/setup 三头 + 数据包）。
    pub fn from_theora_packets(packets: &[Vec<u8>], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Theora, tag);
        if feed_is_single(MediaFormat::Theora, tag) {
            return lazy::theora_single_packets(packets, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Theora, false) {
            return lazy::theora_packets(packets, tag, mode);
        }
        eager::from_theora_packets(packets, tag)
    }

    /// 整文件读入后解码（`video.ogv`，OggS 封装的 theora 磁盘原格式）。
    pub fn from_theora_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        let packets = super::demux::demux_ogg_packets(&buf, tag)?;
        let _ = buf;
        Self::from_theora_packets(&packets, tag)
    }

    /// raw 序列目录（`frameNNNN.raw`，BGRA 直存）：`want` 封顶预载数，
    /// 只收恰好 `frame_bytes` 字节的帧（1920x1080x4，见调用方；半帧不上屏）。
    /// 懒模式按帧文件分块（逐显示帧按需读盘，首帧预读定几何）。
    pub fn from_raw_dir(
        fs: &mut crate::fs::Fs,
        dir: &str,
        want: usize,
        frame_bytes: usize,
        tag: &str,
    ) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Raw, tag);
        if feed_is_single(MediaFormat::Raw, tag) {
            return lazy::raw_single_dir(fs, dir, want, frame_bytes, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, MediaFormat::Raw, false) {
            return lazy::raw_dir(fs, dir, want, frame_bytes, tag, mode);
        }
        eager::from_raw_dir(fs, dir, want, frame_bytes, tag)
    }

    /// raws 单文件字节流解码（`video.raws`，BGRA 帧首尾相接无头）。
    /// 单文件无目录可扫：恒 eager 全量（懒供片的 `single`/`stream`/`zero_copy`
    /// 对 raws 回落 eager，不断播；`decode.raw` 其它四键照常用）。
    pub fn from_raws_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Raw, tag);
        eager::from_raws_bytes(data, tag)
    }

    /// 整文件读入后解码（`video.raws`，单文件 BGRA 序列）。
    pub fn from_raws_file(file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let buf = Self::read_whole_file(file, tag)?;
        Self::from_raws_bytes(&buf, tag)
    }

    /// 极致性能：直接返回当前帧的像素引用，完全无拷贝，无解码。
    /// 懒模式按需解码到窗口后返回（借用契约：调用方须在下次 `next_frame` 前
    /// 用完——`blt/direct_loop` 与 `nano_pump` 都是当帧同步上传，满足）。
    pub fn next_frame(&mut self) -> Option<&[BltPixel]> {
        match self.lazy.as_mut() {
            Some(st) => st.next(&mut self.frames, &mut self.cursor, self.w, self.h),
            None => {
                if self.cursor >= self.frames.len() {
                    return None;
                }
                let frame = &self.frames[self.cursor];
                self.cursor += 1;
                Some(frame)
            }
        }
    }

    /// 索引取帧（壁纸循环/过渡用，越界回绕）。
    /// 懒模式仅窗口内有效（窗口外回退到末帧，不断言不 panic）；
    /// 矩阵强制路径（壁纸/mp_draw/dynamic_draw）恒预解码，走上式。
    pub fn frame(&self, i: usize) -> &[BltPixel] {
        match &self.lazy {
            None => &self.frames[i % self.frames.len().max(1)],
            Some(st) => {
                if !self.frames.is_empty() && i >= st.base() && i - st.base() < self.frames.len() {
                    &self.frames[i - st.base()]
                } else {
                    self.frames.last().map(|f| &f[..]).unwrap_or(&[])
                }
            }
        }
    }

    /// 显示帧总数：eager 下即解码帧数；懒模式下是分块上界（h263 未知取 `MAX`，
    /// 消费一律以 `next_frame` 的 `None` 为准）。
    pub fn len(&self) -> usize {
        match &self.lazy {
            None => self.frames.len(),
            Some(_) => self.total,
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.lazy {
            None => self.frames.is_empty(),
            Some(_) => self.total == 0,
        }
    }

    /// 懒模式重播：重建解码器状态（有状态编解码器丢弃增量上下文，从首分块重走），
    /// 游标归零；eager 下只归零游标。
    pub fn rewind(&mut self) {
        match self.lazy.as_mut() {
            Some(st) => st.reset(&mut self.frames, &mut self.cursor),
            None => self.cursor = 0,
        }
    }

    /// 矩阵强制全量（`mp_draw`/`dynamic_draw` 恒预解码：需全前视做行区切分）：
    /// 把剩余分块全部解出，之后与 clip 全预解码一致。已全量则 no-op。
    /// 失败语义同构造期（空集/坏流直接 Err，调用方按加载失败处理）。
    /// 调用方只有 `video.rs`（load 期裁决），其余调用方零改。
    pub fn ensure_eager(&mut self) -> crate::error::Result {
        if self.lazy.is_none() {
            return Ok(());
        }
        let mut st = self.lazy.take().unwrap_or_else(|| unreachable!());
        let r = st.drain_all(&mut self.frames, self.w, self.h);
        self.total = self.frames.len();
        r
    }

    /// YUV420（8-bit 全范围）→ BGRA 整数量化（BT.601 近似，
    /// 与 `diag::hevc_probe::show_frame` 同式）：
    /// r = y + (359*v >> 8)；g = y - ((88*u + 183*v) >> 8)；
    /// b = y + (454*u >> 8)，u/v 以 128 为中心。
    /// 奇数几何按 `(w+1)/2` 取色度步进并钳位索引（半帧不上屏由调用方保证）。
    pub(crate) fn yuv420_to_bgra(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        w: usize,
        h: usize,
    ) -> Vec<BltPixel> {
        let mut out = Vec::new();
        if out.try_reserve_exact(w.saturating_mul(h)).is_err() {
            return out;
        }
        out.extend(core::iter::repeat_n(
            BltPixel::new(0, 0, 0),
            w.saturating_mul(h),
        ));
        yuv420_to_bgra_into(y, u, v, w, h, &mut out, false);
        out
    }

    /// YUV420 → BGRA（左下原点版，theora 用）：`CroppedFrame` 平面是
    /// lower-left 行序（`samples[y * width + x]`，y 向上），上屏前垂直翻转。
    /// 换算式与 `yuv420_to_bgra` 同式，只有行号取反。
    pub(crate) fn yuv420_to_bgra_vflip(
        y: &[u8],
        u: &[u8],
        v: &[u8],
        w: usize,
        h: usize,
    ) -> Vec<BltPixel> {
        let mut out = Vec::new();
        if out.try_reserve_exact(w.saturating_mul(h)).is_err() {
            return out;
        }
        out.extend(core::iter::repeat_n(
            BltPixel::new(0, 0, 0),
            w.saturating_mul(h),
        ));
        yuv420_to_bgra_into(y, u, v, w, h, &mut out, true);
        out
    }

    /// 整文件读入（各 `from_*_file` 共用：`get_info` 自适应长度 + 一次读全）。
    pub(crate) fn read_whole_file(
        mut file: RegularFile,
        tag: &str,
    ) -> crate::error::Result<Vec<u8>> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                Err(e) => return Err(e.status())?,
            }
        };

        let mut buf = vec![0u8; info.file_size() as usize];
        file.read(&mut buf)?;
        let _ = tag;
        Ok(buf)
    }
}

/// YUV420 → BGRA 复用写（`out` 长度须为 `w*h`，调用方保证；`vflip` 为 theora 行序）。
/// 与 `Clip::yuv420_to_bgra` 同式，供懒 zero_copy 原地复用（零分配稳态）。
///
/// 多分辨率 perf 说明（8 格式调研结论）：1080p 每帧 2M 像素、128x96 小帧每帧
/// 12k 像素，全走本函数标量转换（`copy_width` 只管搬运，不管 YUV 系数）。
/// 三处零语义优化（换算式 `r = y + (359*v >> 8)` 等 BT.601 近似、`clamp`、
/// 缺平面回退 `y=0/uv=128` 全保留）：(1) `vflip` 分支提升为内外双循环
/// （行内无分支）；(2) 行基址提升，内层每像素只剩 `base + col` 单加
/// （`saturating_mul/add` 移出行循环）；(3) 平面裸指针访问（调用方已保
/// `y.len >= w*h`、`uv.len >= uv_stride*h/2` 短写由调用方前置跳过，见
/// `eager.rs`/`source.rs` 的 `ybytes/uvbytes` 门；`debug_assert` 守卫）。
pub(crate) fn yuv420_to_bgra_into(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: usize,
    h: usize,
    out: &mut [BltPixel],
    vflip: bool,
) {
    if w == 0 || h == 0 {
        return;
    }
    let uv_stride = w.div_ceil(2).max(1);
    debug_assert!(out.len() >= w.saturating_mul(h));
    let y_ptr = y.as_ptr();
    let u_ptr = u.as_ptr();
    let v_ptr = v.as_ptr();
    let y_len = y.len();
    let u_len = u.len();
    let v_len = v.len();
    let out_ptr = out.as_mut_ptr();
    // 内层单行转换：`y_base/uv_base/out_base` 均为行首偏移，`col` 只做单加。
    // SAFETY：行窗经调用方 `ybytes/uvbytes` 门保证（短写帧已跳过）；缺尾字节
    // 按旧语义回退（`y=0/uv=128`），`out` 行窗在 `w*h` 内（入口已断言）。
    // 本闭包为安全函数，裸指针解引用逐处 `unsafe`（2024 版 `unsafe_op_in_unsafe_fn` 口径）。
    // 行首偏移 4 个 + 首尾指针/长度 6 个 + 宽 1 个 = 11 个，拆包反而不直观（库内同形先例见 `graphics.rs::blit_rows_direct`）。
    #[allow(clippy::too_many_arguments)]
    fn convert_row(
        y_ptr: *const u8,
        y_len: usize,
        u_ptr: *const u8,
        u_len: usize,
        v_ptr: *const u8,
        v_len: usize,
        y_base: usize,
        uv_base: usize,
        out_ptr: *mut BltPixel,
        out_base: usize,
        w: usize,
    ) {
        for col in 0..w {
            let y_idx = y_base + col;
            // SAFETY：`y_idx < y_len <= 切片 len`，`uv_idx` 同理，`out` 行窗在界内。
            let yy = if y_idx < y_len {
                unsafe { *y_ptr.add(y_idx) as i32 }
            } else {
                0
            };
            let uv_idx = uv_base + col / 2;
            let uu = if uv_idx < u_len {
                unsafe { *u_ptr.add(uv_idx) as i32 - 128 }
            } else {
                0
            };
            let vv = if uv_idx < v_len {
                unsafe { *v_ptr.add(uv_idx) as i32 - 128 }
            } else {
                0
            };
            let r = (yy + ((359 * vv) >> 8)).clamp(0, 255) as u8;
            let g = (yy - ((88 * uu + 183 * vv) >> 8)).clamp(0, 255) as u8;
            let b = (yy + ((454 * uu) >> 8)).clamp(0, 255) as u8;
            unsafe {
                *out_ptr.add(out_base + col) = BltPixel::new(r, g, b);
            }
        }
    }
    if vflip {
        for row in 0..h {
            let src = h - 1 - row;
            let y_base = src * w;
            let uv_base = (src / 2) * uv_stride;
            let out_base = row * w;
            convert_row(
                y_ptr, y_len, u_ptr, u_len, v_ptr, v_len, y_base, uv_base, out_ptr, out_base, w,
            );
        }
    } else {
        for row in 0..h {
            let y_base = row * w;
            let uv_base = (row / 2) * uv_stride;
            convert_row(
                y_ptr, y_len, u_ptr, u_len, v_ptr, v_len, y_base, uv_base, out_ptr, y_base, w,
            );
        }
    }
}

/// BGRA 裸字节零拷贝 reinterpret 为 owned `Vec<BltPixel>`。
/// `BltPixel` 内存布局就是 BGRA（`blue, green, red, reserved`），对齐均为 1，
/// `len % 4 == 0` 已由调用方保证。
pub(crate) fn bytes_as_pixels(bytes: Vec<u8>) -> Vec<BltPixel> {
    debug_assert_eq!(bytes.len() % 4, 0);
    let len = bytes.len() / 4;
    let cap = bytes.capacity() / 4;
    let ptr = bytes.as_ptr() as *mut BltPixel;
    core::mem::forget(bytes);
    unsafe { Vec::from_raw_parts(ptr, len, cap) }
}
