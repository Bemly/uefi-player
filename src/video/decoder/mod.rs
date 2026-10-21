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
//! 缺口（`src/config/types.rs` 已规范：`DecodeThreads`/`DecodeTable`/
//! `FeedMode::Single` 现货，`play.pipeline` + `[render] workers/dist` 已删，
//! 别名已摘；`src/config/mod.rs` 访问子缺→本文件同名垫片过渡，
//! 一俟 `config::decode_threads/zero_copy/streaming/buffers` 落定即转调，
//! 矩阵靠串口 `decode:`/`video: feed=` 行断言，画质不断言）。

use alloc::string::String;
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

/// 有状态软解（顺序解码，其余三项 yes；`qois`/`raw` 非状态，见文件头）。
pub(crate) fn is_stateful_fmt(fmt: MediaFormat) -> bool {
    match fmt {
        MediaFormat::Qois | MediaFormat::Raw => false,
        _ => true,
    }
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

/// `[decode.<fmt>]` 二次解析（`Config` 无此表故 `serde` 忽略，不断整文件；
/// 缺文件/缺表/坏值全回默认，矩阵靠本层 `decode:` 日志断言）。load 期调用
/// （逐 `Clip` 构造几次文件读，非逐帧），失败静默默认。
fn decode_table() -> Option<toml::Table> {
    let mut fs = crate::fs::Fs::new().ok()?;
    let bytes = fs
        .read_file(uefi::cstr16!("player.toml"))
        .unwrap_or_default();
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    toml::from_str::<toml::Table>(&text).ok()
}

fn decode_sub(table: &toml::Table, fmt: MediaFormat) -> Option<toml::Table> {
    let dec = table.get("decode")?.as_table()?;
    dec.get(decode_key(fmt))?.as_table().cloned()
}

/// 规范垫片：`config::decode_threads(fmt) -> DecodeThreads`（缺口见文件头）。
/// 有状态 `all` 解析但 warn 仍 `bsp_only`；`qois`/`raw` 立即兑现。
pub fn decode_threads(fmt: MediaFormat) -> DecodeThreads {
    let want_all = decode_table()
        .and_then(|t| decode_sub(&t, fmt))
        .and_then(|s| s.get("threads").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .map(|s| s.to_ascii_lowercase() == "all")
        .unwrap_or(false);
    if !want_all {
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

/// 规范垫片：`config::decode_zero_copy(fmt) -> bool`（显式开关，缺省 false；
/// `feed=zero_copy` 照常走零拷复用，本项为叠加 hint，稳态零分配见 `lazy`）。
pub fn decode_zero_copy(fmt: MediaFormat) -> bool {
    decode_table()
        .and_then(|t| decode_sub(&t, fmt))
        .and_then(|s| s.get("zero_copy").cloned())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 规范垫片：`config::decode_streaming(fmt) -> bool`（显式开关，缺省 false；
/// `feed=stream` 照常窗口边播边解；`raw` 逐文件 + `h263` reader 恒真流式）。
pub fn decode_streaming(fmt: MediaFormat) -> bool {
    decode_table()
        .and_then(|t| decode_sub(&t, fmt))
        .and_then(|s| s.get("streaming").cloned())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 规范垫片：`config::decode_buffers(fmt) -> u32`（raw 值 0-32：0 跟随
/// `tune.window`，1-32 显式覆盖；钳制 0-32，矩阵靠 `decode:` 行断言）。
pub fn decode_buffers(fmt: MediaFormat) -> u32 {
    let raw = decode_table()
        .and_then(|t| decode_sub(&t, fmt))
        .and_then(|s| s.get("buffers").cloned())
        .and_then(|v| v.as_integer())
        .unwrap_or(0)
        .clamp(0, 32) as u32;
    raw.min(32)
}

/// 每格式解码窗（`buffers=0` 继承全局 `tune.window` 1-32，显式 1-32 覆盖）。
pub(crate) fn decode_window(fmt: MediaFormat) -> usize {
    match decode_buffers(fmt) {
        0 => crate::config::tune_window().clamp(1, 32).max(1) as usize,
        n => n.clamp(1, 32).max(1) as usize,
    }
}

/// `feed="single"` 识别（首选规范 `config::play_feed`，回退 raw 串解析；
/// 壁纸恒 `clip`，见 `lazy::feed_for`）。
pub(crate) fn feed_is_single_raw() -> bool {
    if crate::config::play_feed() == FeedMode::Single {
        return true;
    }
    let t = match decode_table() {
        Some(t) => t,
        None => return false,
    };
    t.get("play")
        .and_then(|p| p.as_table())
        .and_then(|p| p.get("feed"))
        .and_then(|v| v.as_str().map(String::from))
        .map(|s| s.to_ascii_lowercase() == "single")
        .unwrap_or(false)
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
    /// 全预解码构造（`eager.rs` 落点共用）：`lazy` 为空，`total` 即帧数。
    pub(crate) fn eager(w: usize, h: usize, frames: Vec<Vec<BltPixel>>) -> Self {
        let total = frames.len();
        Self {
            w,
            h,
            frames,
            cursor: 0,
            lazy: None,
            total,
        }
    }

    /// qois 字节流解码（`tag` 只进串口：`video` / `wallpaper`）。
    /// 供片裁决见 `lazy::feed_for`（clip 全预解码 / 壁纸恒预解码 / 其余懒解码）。
    /// `single` 先行（raw 串识别，壁纸除外）；解码面单行日志供矩阵 parse 断言。
    pub fn from_qois_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Qois, tag);
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::qois_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, true) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::hevc_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::vp9_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::vp9_single_raw_frames(samples, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
            return lazy::vp9_raw_frames(samples, tag, mode);
        }
        eager::from_vp9_raw_frames(samples, tag)
    }

    /// avc 字节流解码（Annex-B，`tag` 只进串口；MP4-only，无裸流路）。
    pub fn from_avc_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::Avc, tag);
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::avc_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
            return lazy::avc_bytes(data, tag, mode);
        }
        eager::from_avc_bytes(data, tag)
    }

    /// h263 裸流解码（`tag` 只进串口；流式 reader，懒模式免预切）。
    pub fn from_h263_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        log_decode_plane(MediaFormat::H263, tag);
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::h263_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::mpeg2v_single_bytes(data, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::theora_single_packets(packets, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
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
        if tag != "wallpaper" && feed_is_single_raw() {
            return lazy::raw_single_dir(fs, dir, want, frame_bytes, tag);
        }
        if let Some(mode) = lazy::feed_for(tag, false) {
            return lazy::raw_dir(fs, dir, want, frame_bytes, tag, mode);
        }
        eager::from_raw_dir(fs, dir, want, frame_bytes, tag)
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
pub(crate) fn yuv420_to_bgra_into(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: usize,
    h: usize,
    out: &mut [BltPixel],
    vflip: bool,
) {
    let uv_stride = w.div_ceil(2).max(1);
    for row in 0..h {
        let src = if vflip {
            h.saturating_sub(1).saturating_sub(row)
        } else {
            row
        };
        for col in 0..w {
            let yy = y
                .get(src.saturating_mul(w).saturating_add(col))
                .copied()
                .unwrap_or(0) as i32;
            let uv_idx = (src / 2).saturating_mul(uv_stride).saturating_add(col / 2);
            let uu = u.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let vv = v.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let r = (yy + ((359 * vv) >> 8)).clamp(0, 255) as u8;
            let g = (yy - ((88 * uu + 183 * vv) >> 8)).clamp(0, 255) as u8;
            let b = (yy + ((454 * uu) >> 8)).clamp(0, 255) as u8;
            if let Some(px) = out.get_mut(row.saturating_mul(w).saturating_add(col)) {
                *px = BltPixel::new(r, g, b);
            }
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
