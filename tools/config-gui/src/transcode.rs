//! 转换编码页：片源 → 固件可播格式，落在图库（`player.toml` 同目录）。
//!
//! 流程：选源（图库文件/`rfd`/示例片）→ 整轨解复用 → 逐组解码→缩放→编码
//! → 固件布局落盘（`1080p/video.*`；avc/hevc/vp9 全进 MP4）。
//! 编码在 worker 线程跑（`mpsc` 进度回传），UI 只轮询不阻塞；取消用
//! 原子旗标按组检查。内存纪律见 `codecs.rs` 头注释（关键帧分组 GROUP_CAP=120）。
//! 件库约束同其它页（GlassButton/留白，无 TextEdit/ComboBox/滑杆；
//! 路径显示只用 monospace 文本）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use shiguredo_mp4::boxes::{SampleEntry, VisualSampleEntryFields};
use shiguredo_mp4::Uint;

use crate::app::App;
use crate::codecs::*;
use crate::schema::MediaFormat;

/// 转码目标（七档；输出名即固件认的片源文件名，`format` 开关决定播哪个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscodeTarget {
    Avc,
    Hevc,
    Vp9,
    H263,
    Mpeg2v,
    Theora,
    Qois,
}

impl TranscodeTarget {
    pub(crate) const ALL: [Self; 7] = [
        Self::Avc,
        Self::Hevc,
        Self::Vp9,
        Self::H263,
        Self::Mpeg2v,
        Self::Theora,
        Self::Qois,
    ];
    /// 图库相对路径（固件布局；avc/hevc/vp9 同名 video.mp4，
    /// 播哪个由 [play] format 开关决定）。
    pub(crate) fn out_path(self) -> &'static str {
        match self {
            Self::Avc => "1080p/video.mp4",
            Self::Hevc => "1080p/video.mp4",
            Self::Vp9 => "1080p/video.mp4",
            Self::H263 => "1080p/video.h263",
            Self::Mpeg2v => "1080p/video.m2v",
            Self::Theora => "1080p/video.ogv",
            Self::Qois => "1080p/video.qois",
        }
    }
}

/// 转码源白名单（mp4 三轨 + 静帧 + 1080p 裸帧；其余格式后继再接）。
pub(crate) fn source_accepted(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".mp4")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
        || (lower.starts_with("frame") && lower.ends_with(".raw"))
}

/// 进度消息（worker → UI）。
pub(crate) enum JobMsg {
    Progress { done: usize, total: usize },
    Stage(JobStage),
    Finished(Result<JobOut, String>),
}

/// 转码阶段（进度行前缀，UI 侧配文案）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum JobStage {
    Decode,
    Encode,
    Mux,
}

pub(crate) struct JobOut {
    pub(crate) path: String,
    pub(crate) bytes: usize,
}

pub(crate) struct TranscodeJob {
    pub(crate) rx: Receiver<JobMsg>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) handle: Option<std::thread::JoinHandle<()>>,
}

/// 转码状态（`App` 持有；整文件转码，无时间窗）。
pub(crate) struct TranscodeState {
    pub(crate) source: String,
    pub(crate) target: TranscodeTarget,
    pub(crate) info: String,
    pub(crate) status: String,
    pub(crate) job: Option<TranscodeJob>,
    pub(crate) progress: Option<(usize, usize)>,
}

impl TranscodeState {
    pub(crate) fn new() -> Self {
        Self {
            source: String::new(),
            target: TranscodeTarget::Avc,
            info: String::new(),
            status: String::new(),
            job: None,
            progress: None,
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.job.is_some()
    }
}

/// 解码组封顶（关键帧自然分组 + 硬切；120 帧 720p ≈ 170MB 瞬时上限，
/// 超了硬切 + 状态栏不断转码）。
pub(crate) const GROUP_CAP: usize = 120;
/// 编码段（组内再切；30 帧 1080p ≈ 240MB 瞬时上限）。
pub(crate) const ENCODE_CHUNK: usize = 30;
/// 连续解码错误上限（坏流保护，避免无限循环）。
pub(crate) const MAX_DECODE_ERRORS: usize = 16;
/// 输出几何（固件定死 1920x1080）。
pub(crate) const OUT_W: u32 = 1920;
pub(crate) const OUT_H: u32 = 1080;

/// MP4 轨参数（首样片 entry 取参；SPS/PPS 以 Annex-B 前缀存）。
struct TrackMeta {
    id: u32,
    timescale: f64,
    length_size: usize,
    annexb_prefix: Vec<u8>,
}
/// 压缩样片（分组后解码；dur 只用于 fps 估算）。
pub(crate) struct RawSample {
    pub(crate) dur: f64,
    pub(crate) keyframe: bool,
    pub(crate) payload: Vec<u8>,
}
/// 解复用产物（压缩态分组；裸帧不过组，内存封顶见 GROUP_CAP）。
pub(crate) struct DemuxedTrack {
    pub(crate) length_size: usize,
    pub(crate) annexb_prefix: Vec<u8>,
    pub(crate) groups: Vec<Vec<RawSample>>,
    pub(crate) fps: u32,
    pub(crate) total: usize,
}

/// MP4 解复用（want 三轨；关键帧分组 + 硬切封顶；只收压缩负载，不解码）。
pub(crate) fn demux_groups(data: &[u8], want: MediaFormat) -> Result<DemuxedTrack, String> {
    use shiguredo_mp4::boxes::SampleEntry;
    use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
    use shiguredo_mp4::TrackKind;
    let mut dx = Mp4FileDemuxer::new();
    dx.handle_input(Input { position: 0, data });
    let mut meta: Option<TrackMeta> = None;
    let mut kinds: Vec<(u32, MediaFormat)> = Vec::new();
    let mut raws: Vec<RawSample> = Vec::new();
    loop {
        let s = dx.next_sample().map_err(|e| format!("mp4 demux: {e:?}"))?;
        let Some(s) = s else { break };
        if s.track.kind != TrackKind::Video {
            continue;
        }
        // 首样片 entry 定轨类型；entry 缺省沿用同轨结论（与固件同式）。
        let kind = match s.sample_entry {
            Some(SampleEntry::Avc1(_)) => Some(MediaFormat::Avc),
            Some(SampleEntry::Hev1(_)) | Some(SampleEntry::Hvc1(_)) => Some(MediaFormat::Hevc),
            Some(SampleEntry::Vp09(_)) | Some(SampleEntry::Vp08(_)) => Some(MediaFormat::Vp9),
            Some(_) => None,
            None => kinds
                .iter()
                .find(|(id, _)| *id == s.track.track_id)
                .map(|(_, k)| *k),
        };
        let Some(kind) = kind else { continue };
        if !kinds.iter().any(|(id, _)| *id == s.track.track_id) {
            kinds.push((s.track.track_id, kind));
        }
        if kind != want {
            continue;
        }
        if meta.is_none() {
            let (length_size, annexb_prefix) = match s.sample_entry {
                Some(SampleEntry::Avc1(b)) => {
                    let mut p = Vec::new();
                    for sps in &b.avcc_box.sps_list {
                        p.extend_from_slice(&[0, 0, 0, 1]);
                        p.extend_from_slice(sps);
                    }
                    for pps in &b.avcc_box.pps_list {
                        p.extend_from_slice(&[0, 0, 0, 1]);
                        p.extend_from_slice(pps);
                    }
                    ((b.avcc_box.length_size_minus_one.get() + 1) as usize, p)
                }
                Some(SampleEntry::Hev1(b)) => {
                    let mut param = Vec::new();
                    for arr in &b.hvcc_box.nalu_arrays {
                        for nalu in &arr.nalus {
                            param.extend_from_slice(&[0, 0, 0, 1]);
                            param.extend_from_slice(nalu);
                        }
                    }
                    ((b.hvcc_box.length_size_minus_one.get() + 1) as usize, param)
                }
                Some(SampleEntry::Hvc1(b)) => {
                    let mut param = Vec::new();
                    for arr in &b.hvcc_box.nalu_arrays {
                        for nalu in &arr.nalus {
                            param.extend_from_slice(&[0, 0, 0, 1]);
                            param.extend_from_slice(nalu);
                        }
                    }
                    ((b.hvcc_box.length_size_minus_one.get() + 1) as usize, param)
                }
                _ => (4, Vec::new()),
            };
            meta = Some(TrackMeta {
                id: s.track.track_id,
                timescale: s.track.timescale.get() as f64,
                length_size,
                annexb_prefix,
            });
        }
        let m = meta.as_ref().unwrap();
        if s.track.track_id != m.id {
            continue;
        }
        let end = (s.data_offset as usize).saturating_add(s.data_size);
        if end > data.len() {
            continue;
        }
        raws.push(RawSample {
            dur: s.duration as f64 / m.timescale,
            keyframe: s.keyframe,
            payload: data[s.data_offset as usize..end].to_vec(),
        });
    }
    let m = meta.ok_or_else(|| String::from("mp4: no wanted video track"))?;
    if raws.is_empty() {
        return Err(String::from("mp4: no samples in track"));
    }
    // fps：全轨平均样片间隔。
    let total_dur: f64 = raws.iter().map(|r| r.dur).sum();
    let fps = if total_dur > 0.0 {
        (raws.len() as f64 / total_dur).round().clamp(1.0, 120.0) as u32
    } else {
        30
    };
    // 关键帧分组（硬切封顶；裸帧不过组）。
    let mut groups: Vec<Vec<RawSample>> = vec![Vec::new()];
    for r in raws {
        let g = groups.last().unwrap();
        if !g.is_empty() && (r.keyframe || g.len() >= GROUP_CAP) {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(r);
    }
    let total: usize = groups.iter().map(|g| g.len()).sum();
    if total == 0 {
        return Err(String::from("mp4: no samples in track"));
    }
    Ok(DemuxedTrack {
        length_size: m.length_size,
        annexb_prefix: m.annexb_prefix.clone(),
        groups,
        fps,
        total,
    })
}

/// 单轨流式解码器（一把跑完整轨；组是关键帧自然边界 + 硬切封顶，
/// 解码器状态跨组保持，开 GOP 的前向引用不断）。
pub(crate) struct TrackDecoder {
    kind: MediaFormat,
    avc: Option<rusty_h264_decoder::Decoder>,
    hevc: Option<rusty_h265::Decoder>,
    vp9: Option<vp9dec::Decoder>,
    prefix_done: bool,
    w: u32,
    h: u32,
    /// mac VideoToolbox AVC 会话（首组建，跨组复用；Baseline 门控，失败即 CPU）。
    #[cfg(target_os = "macos")]
    vt_avc: Option<shiguredo_video_toolbox::Decoder>,
    #[cfg(target_os = "macos")]
    vt_avc_failed: bool,
    /// Win/Linux Vulkan AVC 会话（首组建，跨组复用；FrameSorter 保序）。
    #[cfg(not(target_os = "macos"))]
    vk_dec: Option<gpu_video::BytesDecoder>,
    #[cfg(not(target_os = "macos"))]
    vk_failed: bool,
}

impl TrackDecoder {
    pub(crate) fn new(kind: MediaFormat) -> Self {
        Self {
            kind,
            avc: matches!(kind, MediaFormat::Avc).then(rusty_h264_decoder::Decoder::new),
            hevc: matches!(kind, MediaFormat::Hevc).then(rusty_h265::Decoder::new),
            vp9: matches!(kind, MediaFormat::Vp9).then(vp9dec::Decoder::new),
            prefix_done: false,
            w: 0,
            h: 0,
            #[cfg(target_os = "macos")]
            vt_avc: None,
            #[cfg(target_os = "macos")]
            vt_avc_failed: false,
            #[cfg(not(target_os = "macos"))]
            vk_dec: None,
            #[cfg(not(target_os = "macos"))]
            vk_failed: false,
        }
    }

    /// 收一帧（几何以首帧为准，其余跳过；调用方只管拼）。
    fn push_frame(
        &mut self,
        w: u32,
        h: u32,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
        out: &mut Vec<Yuv420>,
    ) {
        if self.w == 0 {
            self.w = w;
            self.h = h;
        }
        if w != self.w || h != self.h {
            return;
        }
        out.push(Yuv420 { w, h, y, u, v });
    }
}

/// stride 平面 → 紧凑行（逐行 memcpy，无像素运算；越界即 None）。
/// 转码硬解 AVC（VT Nv12 回读行距常大于宽）用。
fn pack_rows(src: &[u8], stride: usize, w: usize, h: usize) -> Option<Vec<u8>> {
    if stride < w || src.len() < stride * h {
        return None;
    }
    if stride == w {
        return Some(src[..w * h].to_vec());
    }
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        out.extend_from_slice(&src[row * stride..row * stride + w]);
    }
    Some(out)
}

/// mac VideoToolbox AVC 硬解（Nv12 回读 → 拆平面；仅 Baseline SPS，
/// 无 B 帧故与 CPU 同序；非 Baseline/建会话失败返回 None 走 CPU）。
#[cfg(target_os = "macos")]
impl TrackDecoder {
    fn decode_avc_vt(&mut self, d: &DemuxedTrack, g: &[RawSample]) -> Option<Vec<Yuv420>> {
        use shiguredo_video_toolbox::{Decoder, DecoderCodec, DecoderConfig, PixelFormat};
        if self.vt_avc_failed {
            return None;
        }
        if self.vt_avc.is_none() {
            let units = crate::codecs::split_annexb(&d.annexb_prefix);
            if units.len() < 2
                || units[0].is_empty()
                || units[0][0] & 0x1F != 7
                || crate::codecs::sps_profile_idc(&units[0]) != Some(66)
            {
                self.vt_avc_failed = true;
                return None;
            }
            // 会话内已拷贝参数集（video_bg 同式）；进程级常驻，不重复泄漏之忧
            // （转码 job 级 decoder，一次 життя一次建）。
            let sps: &'static [u8] = Box::leak(units[0].to_vec().into_boxed_slice());
            let pps: &'static [u8] = Box::leak(units[1].to_vec().into_boxed_slice());
            match Decoder::new(DecoderConfig {
                codec: DecoderCodec::H264 {
                    sps,
                    pps,
                    nalu_len_bytes: d.length_size as u32,
                },
                pixel_format: PixelFormat::Nv12,
            }) {
                Ok(dec) => self.vt_avc = Some(dec),
                Err(_) => {
                    self.vt_avc_failed = true;
                    return None;
                }
            }
        }
        let mut frames = Vec::new();
        for r in g {
            // 借用内转 owned（帧借 decoder），借用外再 push。
            let got: Option<Option<(usize, usize, Vec<u8>, Vec<u8>)>> = match self.vt_avc.as_mut() {
                Some(dec) => match dec.decode(&r.payload) {
                    Ok(Some(f)) => {
                        // VT 同步解码即时吐帧；Baseline 无重排，到手即显示序。
                        if let shiguredo_video_toolbox::DecodedFrame::Nv12(nv) = f {
                            let (w, h) = (nv.width() as usize, nv.height() as usize);
                            if w == 0 || h == 0 {
                                Some(None)
                            } else {
                                match (
                                    pack_rows(nv.y_plane(), nv.y_stride(), w, h),
                                    pack_rows(nv.uv_plane(), nv.uv_stride(), w, h / 2),
                                ) {
                                    (Some(y), Some(uv)) => Some(Some((w, h, y, uv))),
                                    _ => Some(None),
                                }
                            }
                        } else {
                            Some(None)
                        }
                    }
                    Ok(None) => Some(None),
                    Err(_) => Some(None),
                },
                None => break,
            };
            if let Some(Some((w, h, y, uv))) = got {
                if let Some((u, v)) = crate::codecs::split_uv_planes(&uv, w, h) {
                    self.push_frame(w as u32, h as u32, y, u, v, &mut frames);
                }
            }
        }
        Some(frames)
    }
}

/// Win/Linux Vulkan AVC 硬解（BytesDecoder NV12 回读 → 拆平面；
/// FrameSorter 保序，全 profile 可吃；失败返回 None 走 CPU）。
#[cfg(not(target_os = "macos"))]
impl TrackDecoder {
    fn decode_avc_vk(&mut self, d: &DemuxedTrack, g: &[RawSample]) -> Option<Vec<Yuv420>> {
        if self.vk_failed {
            return None;
        }
        if self.vk_dec.is_none() {
            let dev = crate::codecs::hw_vulkan_device()?;
            match dev.create_bytes_decoder_h264(gpu_video::parameters::DecoderParameters::default())
            {
                Ok(dec) => self.vk_dec = Some(dec),
                Err(_) => {
                    self.vk_failed = true;
                    return None;
                }
            }
        }
        let mut frames = Vec::new();
        for r in g {
            let mut annexb = Vec::new();
            if !self.prefix_done {
                annexb.extend_from_slice(&d.annexb_prefix);
                self.prefix_done = true;
            }
            annexb.extend(crate::codecs::length_prefixed_to_annexb(
                &r.payload,
                d.length_size,
            ));
            // 借用内转 owned，借用外再 push。
            let got: Vec<(usize, usize, Vec<u8>, Vec<u8>, Vec<u8>)> = match self.vk_dec.as_mut() {
                Some(dec) => match dec.decode(gpu_video::EncodedInputChunk {
                    data: &annexb,
                    pts: None,
                }) {
                    Ok(fs) => fs
                        .iter()
                        .filter_map(|f| {
                            let (w, h) = (f.data.width as usize, f.data.height as usize);
                            if w == 0 || h == 0 {
                                return None;
                            }
                            let expect = w * h * 3 / 2;
                            if f.data.frame.len() < expect {
                                return None;
                            }
                            let (y, rest) = f.data.frame.split_at(w * h);
                            let (u, v) = crate::codecs::split_uv_planes(rest, w, h)?;
                            Some((w, h, y.to_vec(), u, v))
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                },
                None => break,
            };
            for (w, h, y, u, v) in got {
                self.push_frame(w as u32, h as u32, y, u, v, &mut frames);
            }
        }
        Some(frames)
    }
}

/// 单组解码（SPS/PPS 前缀只喂首组；HEVC 尾组外不 flush，结束调 finish；
/// 形状与固件一致）。pub(crate)：图库封面复用首帧解码，不另起解码链。
pub(crate) fn decode_group(
    dec: &mut TrackDecoder,
    d: &DemuxedTrack,
    g: &[RawSample],
) -> Result<Vec<Yuv420>, String> {
    let mut annexb = Vec::new();
    if !dec.prefix_done {
        annexb.extend_from_slice(&d.annexb_prefix);
        dec.prefix_done = true;
    }
    let mut frames = Vec::new();
    let mut errs = 0usize;
    match dec.kind {
        MediaFormat::Avc => {
            // 硬解优先（mac VT Nv12 / 非 mac Vulkan NV12；失败回 CPU，不报错）。
            #[cfg(target_os = "macos")]
            if let Some(frames) = dec.decode_avc_vt(d, g) {
                return Ok(frames);
            }
            #[cfg(not(target_os = "macos"))]
            if let Some(frames) = dec.decode_avc_vk(d, g) {
                return Ok(frames);
            }
            for r in g {
                annexb.extend(length_prefixed_to_annexb(&r.payload, d.length_size));
            }
            let decoded = match dec.avc.as_mut() {
                Some(a) => a
                    .decode_stream(&annexb)
                    .map_err(|e| format!("avc decode: {e:?}"))?,
                None => return Err(String::from("avc: no decoder")),
            };
            for f in &decoded {
                let planes = f.as_planes();
                if !planes.is_tight() {
                    continue;
                }
                dec.push_frame(
                    f.width as u32,
                    f.height as u32,
                    planes.y.to_vec(),
                    planes.u.to_vec(),
                    planes.v.to_vec(),
                    &mut frames,
                );
            }
            Ok(frames)
        }
        MediaFormat::Hevc => {
            for r in g {
                annexb.extend(length_prefixed_to_annexb(&r.payload, d.length_size));
            }
            match dec.hevc.as_mut() {
                Some(h) => h
                    .push_annexb(&annexb, None)
                    .map_err(|e| format!("hevc push: {e:?}"))?,
                None => return Err(String::from("hevc: no decoder")),
            }
            loop {
                let f = match dec.hevc.as_mut() {
                    Some(h) => h.next_frame(),
                    None => return Err(String::from("hevc: no decoder")),
                };
                let f = match f {
                    Ok(f) => f,
                    Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
                    Err(_) => {
                        errs += 1;
                        if errs >= MAX_DECODE_ERRORS {
                            break;
                        }
                        continue;
                    }
                };
                if f.bit_depth() != 8 {
                    continue;
                }
                let w = f.width as u32;
                let h = f.height as u32;
                let mut yuv = Vec::new();
                f.write_yuv(&mut yuv);
                let ybytes = w as usize * h as usize;
                let uvbytes = (w as usize).div_ceil(2) * (h as usize).div_ceil(2);
                if yuv.len() < ybytes + uvbytes * 2 {
                    continue;
                }
                let (ypl, rest) = yuv.split_at(ybytes);
                let (upl, vpl) = rest.split_at(uvbytes.min(rest.len()));
                if vpl.len() < uvbytes {
                    continue;
                }
                dec.push_frame(w, h, ypl.to_vec(), upl.to_vec(), vpl.to_vec(), &mut frames);
            }
            Ok(frames)
        }
        MediaFormat::Vp9 => {
            for r in g {
                let decoded = match dec.vp9.as_mut() {
                    Some(v) => v
                        .decode_frame(&r.payload)
                        .map_err(|e| format!("vp9 decode: {e:?}"))?,
                    None => return Err(String::from("vp9: no decoder")),
                };
                for d in decoded {
                    let Some(f) = d.frame else { continue };
                    if f.bit_depth != 8 || f.subsampling_x != 1 || f.subsampling_y != 1 {
                        continue;
                    }
                    let (
                        vp9dec::PlaneData::U8(y),
                        vp9dec::PlaneData::U8(u),
                        vp9dec::PlaneData::U8(v),
                    ) = (&f.y, &f.u, &f.v)
                    else {
                        continue;
                    };
                    dec.push_frame(
                        f.width,
                        f.height,
                        y.clone(),
                        u.clone(),
                        v.clone(),
                        &mut frames,
                    );
                }
            }
            Ok(frames)
        }
        _ => Err(String::from("mp4: unsupported track")),
    }
}

/// 收尾（HEVC flush 后排空；Vulkan AVC 解码器同理排空；avc/vp9 CPU 无缓冲）。
fn finish_decode(dec: &mut TrackDecoder) -> Result<Vec<Yuv420>, String> {
    let mut frames = Vec::new();
    #[cfg(not(target_os = "macos"))]
    if dec.kind == MediaFormat::Avc {
        // Vulkan BytesDecoder 内 FrameSorter 可能滞留 B 帧重排尾巴，flush 吐完。
        if let Some(b) = dec.vk_dec.as_mut() {
            match b.flush() {
                Ok(fs) => {
                    for f in &fs {
                        let (w, h) = (f.data.width as usize, f.data.height as usize);
                        if w == 0 || h == 0 || f.data.frame.len() < w * h * 3 / 2 {
                            continue;
                        }
                        let (y, rest) = f.data.frame.split_at(w * h);
                        if let Some((u, v)) = crate::codecs::split_uv_planes(rest, w, h) {
                            dec.push_frame(w as u32, h as u32, y.to_vec(), u, v, &mut frames);
                        }
                    }
                }
                Err(_) => {}
            }
        }
        return Ok(frames);
    }
    if dec.hevc.is_none() {
        return Ok(frames);
    }
    if let Some(hevc) = dec.hevc.as_mut() {
        hevc.flush();
    }
    let mut errs = 0usize;
    loop {
        let f = match dec.hevc.as_mut() {
            Some(h) => h.next_frame(),
            None => return Err(String::from("hevc: no decoder")),
        };
        let f = match f {
            Ok(f) => f,
            Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
            Err(_) => {
                errs += 1;
                if errs >= MAX_DECODE_ERRORS {
                    break;
                }
                continue;
            }
        };
        if f.bit_depth() != 8 {
            continue;
        }
        let w = f.width as u32;
        let h = f.height as u32;
        let mut yuv = Vec::new();
        f.write_yuv(&mut yuv);
        let ybytes = w as usize * h as usize;
        let uvbytes = (w as usize).div_ceil(2) * (h as usize).div_ceil(2);
        if yuv.len() < ybytes + uvbytes * 2 {
            continue;
        }
        let (ypl, rest) = yuv.split_at(ybytes);
        let (upl, vpl) = rest.split_at(uvbytes.min(rest.len()));
        if vpl.len() < uvbytes {
            continue;
        }
        dec.push_frame(w, h, ypl.to_vec(), upl.to_vec(), vpl.to_vec(), &mut frames);
    }
    Ok(frames)
}

impl App {
    /// 选源后立即探针（同步，快）：行内直接看到“是什么片子”，不抽象。
    pub(crate) fn refresh_probe(&mut self) {
        if self.transcode.source.is_empty() {
            self.transcode.info.clear();
            return;
        }
        match probe_source(&self.transcode.source) {
            Ok(info) => {
                self.transcode.info = info.summary();
                self.transcode.status.clear();
            }
            Err(e) => {
                self.transcode.info.clear();
                self.transcode.status = format!("probe: {e}");
            }
        }
    }

    /// 转码页：源行 + 探针行 + 目标行 + 输出行 + 编码按钮 + 状态。
    pub(crate) fn transcode_page(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        use rust_i18n::t;
        // 先收 worker 进度（UI 线程只 try_recv，不阻塞）。
        self.poll_transcode_job();
        // 任务跑着就一直要帧：eframe 按需重绘，不主动要就永远停在
        // 点击那一帧，进度/完成/报错全都刷不出来（“0/0 卡死”的真凶之一）。
        if self.transcode.running() {
            ui.ctx().request_repaint();
        }
        self.content_heading(ui, t!("transcode.heading"));
        self.content_small(ui, t!("transcode.hint"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            self.content_label(ui, t!("transcode.source"));
            let shown = if self.transcode.source.is_empty() {
                t!("transcode.no_source").into_owned()
            } else {
                self.transcode.source.clone()
            };
            self.content_monospace(ui, shown);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if egui_glass::GlassButton::new(t!("transcode.pick"))
                .show(ui)
                .clicked()
            {
                let dir = crate::gallery::gallery_dir(&self.path);
                if let Some(f) = rfd::FileDialog::new()
                    .set_directory(dir)
                    .add_filter("video/image", &["mp4", "png", "jpg", "jpeg", "bmp", "raw"])
                    .pick_file()
                {
                    self.transcode.source = f.display().to_string();
                    self.refresh_probe();
                }
            }
            if egui_glass::GlassButton::new(t!("transcode.example"))
                .show(ui)
                .clicked()
            {
                self.transcode.source = String::from("/Users/bemly/Movies/input.mp4");
                self.refresh_probe();
            }
            if egui_glass::GlassButton::new(t!("transcode.from_gallery"))
                .show(ui)
                .clicked()
            {
                self.page = crate::app::Page::Gallery;
            }
        });
        ui.add_space(10.0);
        self.content_label(ui, t!("transcode.target"));
        ui.horizontal_wrapped(|ui| {
            for v in TranscodeTarget::ALL {
                if self.option_button(
                    ui,
                    self.transcode.target == v,
                    &crate::text::transcode_target_name(v),
                ) {
                    self.transcode.target = v;
                }
            }
        });
        self.content_small(
            ui,
            crate::text::transcode_target_desc(self.transcode.target),
        );
        ui.add_space(10.0);
        if !self.transcode.info.is_empty() {
            ui.horizontal(|ui| {
                self.content_label(ui, t!("transcode.probe"));
                self.content_monospace(ui, self.transcode.info.as_str());
            });
            ui.add_space(4.0);
        }
        let out_rel = self.transcode.target.out_path();
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(out_rel)
            .display()
            .to_string();
        ui.horizontal(|ui| {
            self.content_label(ui, t!("transcode.output"));
            self.content_monospace(ui, out_full.as_str());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = if self.transcode.running() {
                t!("transcode.cancel")
            } else {
                t!("transcode.encode")
            };
            if egui_glass::GlassButton::new(label).show(ui).clicked() {
                if self.transcode.running() {
                    self.cancel_transcode_job();
                } else {
                    self.start_transcode_job();
                }
            }
            if let Some((done, total)) = self.transcode.progress {
                // 玻璃库无进度件（egui_glass 0.1.5 只有 Glass/Button/Toolbar/Slider，
                // 本地 registry 源码确认过），纯展示条用原语 ProgressBar（非交互件，
                // 不在禁用之列；嵌在同一行内，不另起 Panel）。
                let frac = if total > 0 {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!("{done}/{total}"))
                        .desired_width(180.0),
                );
            }
        });
        if !self.transcode.status.is_empty() {
            ui.add_space(4.0);
            self.content_small(ui, self.transcode.status.as_str());
        }
    }

    /// worker 进度轮询 + 收尾（worker 线程落盘，UI 只报状态）。
    pub(crate) fn poll_transcode_job(&mut self) {
        use rust_i18n::t;
        use std::sync::mpsc::TryRecvError;
        let mut finished: Option<Result<JobOut, String>> = None;
        let mut progress = None;
        let mut dead = false;
        if let Some(job) = self.transcode.job.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(JobMsg::Progress { done, total }) => progress = Some((done, total)),
                    Ok(JobMsg::Stage(s)) => {
                        self.transcode.status = match s {
                            JobStage::Decode => t!("transcode.stage_decode").into_owned(),
                            JobStage::Encode => t!("transcode.stage_encode").into_owned(),
                            JobStage::Mux => t!("transcode.stage_mux").into_owned(),
                        };
                    }
                    Ok(JobMsg::Finished(r)) => {
                        finished = Some(r);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    // 发送端没了但没发 Finished = worker 线程 panic 或提前退出，
                    // 不能停在“取消/解码中”装活。
                    Err(TryRecvError::Disconnected) => {
                        dead = true;
                        break;
                    }
                }
            }
        }
        if let Some(p) = progress {
            self.transcode.progress = Some(p);
        }
        if dead && finished.is_none() {
            finished = Some(Err(String::from(
                "transcode worker gone without finish (likely panicked; see log)",
            )));
        }
        if let Some(r) = finished {
            // 线程已发 Finished 即将退出，join 不阻塞；句柄收走免 detached 堆积。
            if let Some(job) = self.transcode.job.as_mut() {
                if let Some(h) = job.handle.take() {
                    let _ = h.join();
                }
            }
            self.transcode.job = None;
            self.transcode.progress = None;
            match r {
                Ok(out) => {
                    self.transcode.status = format!("done: {} ({} bytes)", out.path, out.bytes);
                }
                Err(e) => {
                    self.transcode.status = format!("error: {e}");
                }
            }
        }
    }

    pub(crate) fn cancel_transcode_job(&mut self) {
        if let Some(job) = self.transcode.job.as_ref() {
            job.cancel.store(true, Ordering::Relaxed);
            self.transcode.status = String::from("cancelling...");
        }
    }

    pub(crate) fn start_transcode_job(&mut self) {
        use rust_i18n::t;
        if self.transcode.source.is_empty() {
            self.transcode.status = t!("transcode.need_source").into_owned();
            return;
        }
        if !source_accepted(&self.transcode.source) {
            self.transcode.status = format!(
                "unsupported source (want .mp4/.png/.jpg/.bmp/.raw): {}",
                self.transcode.source
            );
            return;
        }
        // 先探片源（同步，快：只扫容器头 + 样片表）：行内直接报具体规格。
        match probe_source(&self.transcode.source) {
            Ok(info) => self.transcode.info = info.summary(),
            Err(e) => {
                self.transcode.status = format!("probe: {e}");
                self.transcode.info.clear();
                return;
            }
        }
        let source = self.transcode.source.clone();
        let out_rel = self.transcode.target.out_path().to_string();
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(&out_rel)
            .display()
            .to_string();
        let target = self.transcode.target;
        let (tx, rx): (Sender<JobMsg>, Receiver<JobMsg>) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let tx2 = tx.clone();
        let tx3 = tx.clone();
        let handle = std::thread::spawn(move || {
            let stage = move |s: JobStage| {
                let _ = tx2.send(JobMsg::Stage(s));
            };
            let progress = move |d: usize, t: usize| {
                let _ = tx3.send(JobMsg::Progress { done: d, total: t });
            };
            // panic 也必须落袋为 Finished(Err)，否则 UI 永远停在“解码中”。
            let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_transcode_job(&source, target, &out_full, &cancel_w, stage, progress)
            })) {
                Ok(inner) => inner,
                Err(_) => Err(String::from("transcode worker panicked")),
            };
            let _ = tx.send(JobMsg::Finished(r));
        });
        self.transcode.job = Some(TranscodeJob {
            rx,
            cancel,
            handle: Some(handle),
        });
        self.transcode.progress = Some((0, 0));
        self.transcode.status = t!("transcode.running").into_owned();
    }
}

/// 编码累积器（流式：逐段 push，最后 finish 一次性包装；裸帧不过段）。
#[derive(Default)]
struct EncodeAcc {
    avc: Option<AvcStream>,
    hevc: Option<HevcStream>,
    vp9: Vec<Vp9PacketData>,
    theora_headers: Vec<Vec<u8>>,
    theora_packets: Vec<Vec<u8>>,
    bare: Vec<u8>,
    qois: Vec<u8>,
}

impl EncodeAcc {
    fn push(&mut self, target: TranscodeTarget, g: &[Yuv420]) -> Result<(), String> {
        match target {
            TranscodeTarget::Avc => {
                let s = encode_avc(g)?;
                if self.avc.is_none() {
                    self.avc = Some(AvcStream {
                        sps: s.sps.clone(),
                        pps: s.pps.clone(),
                        samples: Vec::new(),
                    });
                }
                self.avc.as_mut().unwrap().samples.extend(s.samples);
            }
            TranscodeTarget::Hevc => {
                let s = encode_hevc(g)?;
                if self.hevc.is_none() {
                    self.hevc = Some(HevcStream {
                        vps: s.vps.clone(),
                        sps: s.sps.clone(),
                        pps: s.pps.clone(),
                        samples: Vec::new(),
                    });
                }
                self.hevc.as_mut().unwrap().samples.extend(s.samples);
            }
            TranscodeTarget::Vp9 => self.vp9.extend(encode_vp9(g)?),
            TranscodeTarget::Theora => {
                let t = encode_theora(g)?;
                if self.theora_headers.is_empty() {
                    self.theora_headers = t.header_packets;
                }
                self.theora_packets.extend(t.data_packets);
            }
            TranscodeTarget::H263 => self.bare.extend(encode_h263(g)?),
            TranscodeTarget::Mpeg2v => self.bare.extend(encode_mpeg2v(g)?),
            TranscodeTarget::Qois => {
                for f in g {
                    let rgba = yuv420_to_rgba(f);
                    let q = qoi::encode_to_vec(&rgba.px, rgba.w, rgba.h)
                        .map_err(|e| format!("qoi: {e:?}"))?;
                    self.qois.extend_from_slice(&(q.len() as u32).to_le_bytes());
                    self.qois.extend_from_slice(&q);
                }
            }
        }
        Ok(())
    }

    fn finish(self, target: TranscodeTarget, fps: u32) -> Result<Vec<u8>, String> {
        match target {
            TranscodeTarget::Avc => {
                let s = self.avc.ok_or_else(|| String::from("avc: no stream"))?;
                let entry = avc_entry(&s.sps, &s.pps, OUT_W, OUT_H);
                let samples: Vec<Mp4OutSample> = s
                    .samples
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: length_prefixed_nals(&x.nalus),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, OUT_W, OUT_H, fps)
            }
            TranscodeTarget::Hevc => {
                let s = self.hevc.ok_or_else(|| String::from("hevc: no stream"))?;
                let entry = hevc_entry(&s.vps, &s.sps, &s.pps, OUT_W, OUT_H, fps);
                let samples: Vec<Mp4OutSample> = s
                    .samples
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: length_prefixed_nals(&x.nalus),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, OUT_W, OUT_H, fps)
            }
            TranscodeTarget::Vp9 => {
                let entry = vp9_entry(OUT_W, OUT_H);
                let samples: Vec<Mp4OutSample> = self
                    .vp9
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: x.data.clone(),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, OUT_W, OUT_H, fps)
            }
            TranscodeTarget::Theora => mux_ogg_theora(&self.theora_headers, &self.theora_packets),
            TranscodeTarget::H263 | TranscodeTarget::Mpeg2v => Ok(self.bare),
            TranscodeTarget::Qois => Ok(self.qois),
        }
    }
}

/// 后台转码主流程（worker 线程内跑；`stage` 报阶段，`progress` 报帧进度）。
/// 流式：demux 分组 → 逐组解码→缩放→编码→释放（裸帧不过组，内存封顶）。
#[allow(clippy::too_many_lines)]
pub(crate) fn run_transcode_job(
    source: &str,
    target: TranscodeTarget,
    out_path: &str,
    cancel: &AtomicBool,
    mut stage: impl FnMut(JobStage),
    mut progress: impl FnMut(usize, usize),
) -> Result<JobOut, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("read source: {e}"))?;
    let lower = source.to_ascii_lowercase();
    stage(JobStage::Decode);
    let mut acc = EncodeAcc::default();
    let mut done = 0usize;
    let mut encoding = false;
    let fps: u32;
    if lower.ends_with(".mp4") {
        // 源轨按容器头选：首个有样片的 avc/hevc/vp9 轨（与固件同顺序）。
        let mut demuxed = None;
        for want in [MediaFormat::Avc, MediaFormat::Hevc, MediaFormat::Vp9] {
            match demux_groups(&bytes, want) {
                Ok(d) if d.total > 0 => {
                    demuxed = Some((want, d));
                    break;
                }
                _ => continue,
            }
        }
        let (want, d) = demuxed.ok_or_else(|| String::from("mp4: no avc/hevc/vp9 track"))?;
        fps = d.fps;
        let total = d.total;
        let mut dec = TrackDecoder::new(want);
        for g in &d.groups {
            if cancel.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
            let frames = decode_group(&mut dec, &d, g)?;
            if !encoding && !frames.is_empty() {
                stage(JobStage::Encode);
                encoding = true;
            }
            for chunk in frames.chunks(ENCODE_CHUNK) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(String::from("cancelled"));
                }
                let scaled: Vec<Yuv420> = chunk
                    .iter()
                    .map(|f| scale_yuv420(f, OUT_W, OUT_H))
                    .collect();
                acc.push(target, &scaled)?;
                done += chunk.len();
                progress(done, total);
            }
        }
        // 尾部排空（HEVC flush；其余空）。
        let tail = finish_decode(&mut dec)?;
        if !tail.is_empty() {
            stage(JobStage::Encode);
            for chunk in tail.chunks(ENCODE_CHUNK) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(String::from("cancelled"));
                }
                let scaled: Vec<Yuv420> = chunk
                    .iter()
                    .map(|f| scale_yuv420(f, OUT_W, OUT_H))
                    .collect();
                acc.push(target, &scaled)?;
                done += chunk.len();
                progress(done, total);
            }
        }
        if done == 0 {
            return Err(String::from("mp4: no decodable frames"));
        }
    } else {
        // 静帧/裸帧：单帧直接进同一编码循环。
        let rgba = if lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".bmp")
        {
            let img = image::open(std::path::Path::new(source))
                .map_err(|e| format!("image decode: {e}"))?
                .to_rgba8();
            let (w, h) = (img.width(), img.height());
            RgbaFrame {
                w,
                h,
                px: img.into_raw(),
            }
        } else if lower.ends_with(".raw") {
            if bytes.len() != 1920 * 1080 * 4 {
                return Err(format!(
                    "raw: want 1920x1080 BGRA, got {} bytes",
                    bytes.len()
                ));
            }
            let mut px = vec![0u8; bytes.len()];
            for (i, p) in bytes.as_chunks::<4>().0.iter().enumerate() {
                px[i * 4] = p[2];
                px[i * 4 + 1] = p[1];
                px[i * 4 + 2] = p[0];
                px[i * 4 + 3] = p[3];
            }
            RgbaFrame {
                w: 1920,
                h: 1080,
                px,
            }
        } else {
            return Err(format!("unsupported source: {source}"));
        };
        let frames = [rgba_to_yuv420(&rgba)];
        fps = 30;
        let total = frames.len();
        stage(JobStage::Encode);
        for chunk in frames.chunks(ENCODE_CHUNK) {
            let scaled: Vec<Yuv420> = chunk
                .iter()
                .map(|f| scale_yuv420(f, OUT_W, OUT_H))
                .collect();
            acc.push(target, &scaled)?;
            done += chunk.len();
            progress(done, total);
        }
    }
    // 容器包装 + 落盘。
    stage(JobStage::Mux);
    let bytes = acc.finish(target, fps)?;

    if let Some(parent) = std::path::Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
    }
    std::fs::write(out_path, &bytes).map_err(|e| format!("write: {e}"))?;
    Ok(JobOut {
        path: out_path.to_string(),
        bytes: bytes.len(),
    })
}

/// 片源探针结果（选源时同步快探：只扫容器头 + 样片表，不解码）。
pub(crate) struct ProbeInfo {
    pub(crate) container: &'static str,
    pub(crate) codec: &'static str,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) fps: u32,
    pub(crate) n: usize,
    pub(crate) dur_s: u64,
}

impl ProbeInfo {
    pub(crate) fn summary(&self) -> String {
        format!(
            "{} {} {}x{} {}fps {}帧 ~{}s",
            self.container, self.codec, self.w, self.h, self.fps, self.n, self.dur_s
        )
    }
}

/// 探片源（mp4 看首个视频轨 + 样片计数；静帧看尺寸；raw 定死 1080p）。
pub(crate) fn probe_source(path: &str) -> Result<ProbeInfo, String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".mp4") {
        let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
        return probe_mp4(&data);
    }
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
    {
        let (w, h) = image::image_dimensions(path).map_err(|e| format!("image: {e}"))?;
        return Ok(ProbeInfo {
            container: "image",
            codec: "rgba",
            w,
            h,
            fps: 30,
            n: 1,
            dur_s: 0,
        });
    }
    if lower.ends_with(".raw") {
        let len = std::fs::metadata(path)
            .map_err(|e| format!("stat: {e}"))?
            .len();
        if len != (1920 * 1080 * 4) as u64 {
            return Err(format!("raw: want 1920x1080 BGRA, got {len} bytes"));
        }
        return Ok(ProbeInfo {
            container: "raw",
            codec: "bgra",
            w: 1920,
            h: 1080,
            fps: 30,
            n: 1,
            dur_s: 0,
        });
    }
    Err(format!("unsupported source: {path}"))
}

/// MP4 探针（首个 avc/hevc/vp9 视频轨 + 样片计数；负载不拷贝）。
fn probe_mp4(data: &[u8]) -> Result<ProbeInfo, String> {
    use shiguredo_mp4::boxes::SampleEntry;
    use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
    use shiguredo_mp4::TrackKind;
    let mut dx = Mp4FileDemuxer::new();
    dx.handle_input(Input { position: 0, data });
    let mut found: Option<(MediaFormat, u32, u32)> = None;
    let mut target_id = 0u32;
    let mut kinds: Vec<(u32, MediaFormat)> = Vec::new();
    let mut n = 0usize;
    let mut total_dur = 0f64;
    let mut timescale = 1f64;
    loop {
        let s = dx.next_sample().map_err(|e| format!("mp4 demux: {e:?}"))?;
        let Some(s) = s else { break };
        if s.track.kind != TrackKind::Video {
            continue;
        }
        // 首样片 entry 定轨类型；entry 缺省沿用同轨结论（mux 只在首样片带 entry）。
        let kind = match s.sample_entry {
            Some(SampleEntry::Avc1(_)) => Some(MediaFormat::Avc),
            Some(SampleEntry::Hev1(_)) | Some(SampleEntry::Hvc1(_)) => Some(MediaFormat::Hevc),
            Some(SampleEntry::Vp09(_)) | Some(SampleEntry::Vp08(_)) => Some(MediaFormat::Vp9),
            Some(_) => None,
            None => kinds
                .iter()
                .find(|(id, _)| *id == s.track.track_id)
                .map(|(_, k)| *k),
        };
        let Some(kind) = kind else { continue };
        if !kinds.iter().any(|(id, _)| *id == s.track.track_id) {
            kinds.push((s.track.track_id, kind));
        }
        if found.is_none() {
            let (w, h) = match s.sample_entry {
                Some(SampleEntry::Avc1(b)) => (b.visual.width as u32, b.visual.height as u32),
                Some(SampleEntry::Hev1(b)) => (b.visual.width as u32, b.visual.height as u32),
                Some(SampleEntry::Hvc1(b)) => (b.visual.width as u32, b.visual.height as u32),
                Some(SampleEntry::Vp09(b)) => (b.visual.width as u32, b.visual.height as u32),
                Some(SampleEntry::Vp08(b)) => (b.visual.width as u32, b.visual.height as u32),
                _ => (0, 0),
            };
            found = Some((kind, w, h));
            target_id = s.track.track_id;
            timescale = s.track.timescale.get() as f64;
        }
        if s.track.track_id != target_id {
            continue;
        }
        n += 1;
        total_dur += s.duration as f64 / timescale;
    }
    let (kind, w, h) = found.ok_or_else(|| String::from("mp4: no avc/hevc/vp9 video track"))?;
    let fps = if total_dur > 0.0 {
        (n as f64 / total_dur).round().clamp(1.0, 120.0) as u32
    } else {
        30
    };
    let codec = match kind {
        MediaFormat::Avc => "avc",
        MediaFormat::Hevc => "hevc",
        MediaFormat::Vp9 => "vp9",
        _ => "?",
    };
    Ok(ProbeInfo {
        container: "mp4",
        codec,
        w,
        h,
        fps,
        n,
        dur_s: total_dur.round() as u64,
    })
}

/// MP4 输出样片（负载形态按编码定：avc/hevc 长度前缀，vp9 裸帧）。
pub(crate) struct Mp4OutSample {
    pub(crate) keyframe: bool,
    pub(crate) payload: Vec<u8>,
}

/// NAL 串 → 长度前缀样片负载（MP4 样片标准形态）。
pub(crate) fn length_prefixed_nals(nalus: &[Vec<u8>]) -> Vec<u8> {
    let mut pl = Vec::new();
    for nalu in nalus {
        pl.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        pl.extend_from_slice(nalu);
    }
    pl
}

fn visual_fields(w: u32, h: u32) -> VisualSampleEntryFields {
    use shiguredo_mp4::boxes::VisualSampleEntryFields;
    VisualSampleEntryFields {
        data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
        width: w as u16,
        height: h as u16,
        horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
        vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
        frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
        compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
        depth: VisualSampleEntryFields::DEFAULT_DEPTH,
    }
}

/// AVC entry（`avc1/avcC`，profile/level 从 SPS 读）。
pub(crate) fn avc_entry(sps: &[u8], pps: &[u8], w: u32, h: u32) -> SampleEntry {
    use shiguredo_mp4::boxes::{Avc1Box, AvccBox};
    SampleEntry::Avc1(Avc1Box {
        visual: visual_fields(w, h),
        avcc_box: AvccBox {
            avc_profile_indication: sps.get(1).copied().unwrap_or(66),
            profile_compatibility: sps.get(2).copied().unwrap_or(0),
            avc_level_indication: sps.get(3).copied().unwrap_or(30),
            length_size_minus_one: Uint::new(3),
            sps_list: vec![sps.to_vec()],
            pps_list: vec![pps.to_vec()],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext_list: vec![],
        },
        unknown_boxes: vec![],
    })
}

/// HEVC entry（`hvc1/hvcC`；固件 demux 不校验 profile 字段，
/// 此处按 Main 8-bit 420 填保守值，level 随帧率取 4.0/4.1）。
pub(crate) fn hevc_entry(
    vps: &[u8],
    sps: &[u8],
    pps: &[u8],
    w: u32,
    h: u32,
    fps: u32,
) -> SampleEntry {
    use shiguredo_mp4::boxes::{Hvc1Box, HvccBox, HvccNalUintArray};
    let array = |t: u8, nalus: Vec<Vec<u8>>| HvccNalUintArray {
        array_completeness: Uint::new(1),
        nal_unit_type: Uint::new(t),
        nalus,
    };
    SampleEntry::Hvc1(Hvc1Box {
        visual: visual_fields(w, h),
        hvcc_box: HvccBox {
            general_profile_space: Uint::new(0),
            general_tier_flag: Uint::new(0),
            general_profile_idc: Uint::new(1),
            general_profile_compatibility_flags: 0x6000_0000,
            general_constraint_indicator_flags: Uint::new(0),
            general_level_idc: if fps > 30 { 123 } else { 120 },
            min_spatial_segmentation_idc: Uint::new(0),
            parallelism_type: Uint::new(0),
            chroma_format_idc: Uint::new(1),
            bit_depth_luma_minus8: Uint::new(0),
            bit_depth_chroma_minus8: Uint::new(0),
            avg_frame_rate: (fps.saturating_mul(256)) as u16,
            constant_frame_rate: Uint::new(0),
            num_temporal_layers: Uint::new(1),
            temporal_id_nested: Uint::new(1),
            length_size_minus_one: Uint::new(3),
            nalu_arrays: vec![
                array(32, vec![vps.to_vec()]),
                array(33, vec![sps.to_vec()]),
                array(34, vec![pps.to_vec()]),
            ],
        },
        unknown_boxes: vec![],
    })
}

/// VP9 entry（`vp09/vpcC`；裸帧样片，固件直送解码器）。
pub(crate) fn vp9_entry(w: u32, h: u32) -> SampleEntry {
    use shiguredo_mp4::boxes::{Vp09Box, VpccBox};
    SampleEntry::Vp09(Vp09Box {
        visual: visual_fields(w, h),
        vpcc_box: VpccBox {
            profile: 0,
            level: 41,
            bit_depth: Uint::new(8),
            chroma_subsampling: Uint::new(1),
            video_full_range_flag: Uint::new(1),
            colour_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            codec_initialization_data: Vec::new(),
        },
        unknown_boxes: vec![],
    })
}

/// MP4 打包通用路（`Mp4FileMuxer`；entry 决定 avc1/hvc1/vp09）。
pub(crate) fn mux_mp4_video(
    entry: SampleEntry,
    samples: &[Mp4OutSample],
    w: u32,
    h: u32,
    fps: u32,
) -> Result<Vec<u8>, String> {
    use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};
    use shiguredo_mp4::TrackKind;
    use std::num::NonZeroU32;
    let _ = (w, h);
    if samples.is_empty() {
        return Err(String::from("mp4: no samples"));
    }
    let mut muxer = Mp4FileMuxer::new().map_err(|e| format!("mux init: {e:?}"))?;
    let timescale = NonZeroU32::new(90000).unwrap();
    let dur = 90000u32 / fps.max(1);
    let mut data_offset = muxer.initial_boxes_bytes().len() as u64;
    for (i, s) in samples.iter().enumerate() {
        muxer
            .append_sample(&Sample {
                track_kind: TrackKind::Video,
                sample_entry: if i == 0 { Some(entry.clone()) } else { None },
                keyframe: s.keyframe,
                timescale,
                duration: dur,
                composition_time_offset: None,
                data_offset,
                data_size: s.payload.len(),
            })
            .map_err(|e| format!("mux append: {e:?}"))?;
        data_offset += s.payload.len() as u64;
    }
    let initial = muxer.initial_boxes_bytes().to_vec();
    let finalized = muxer
        .finalize()
        .map_err(|e| format!("mux finalize: {e:?}"))?;
    // 组装：head（pairs 改写）+ mdat 头（initial 尾部 16B，pairs 改写终值）+
    // 样片负载（紧跟 initial）+ moov（pairs 偏移）。
    let mut max_end = initial.len() + samples.iter().map(|s| s.payload.len()).sum::<usize>();
    for (off, b) in finalized.offset_and_bytes_pairs() {
        max_end = max_end.max(off as usize + b.len());
    }
    let mut file = vec![0u8; max_end];
    file[..initial.len()].copy_from_slice(&initial);
    for (off, b) in finalized.offset_and_bytes_pairs() {
        let o = off as usize;
        file[o..o + b.len()].copy_from_slice(b);
    }
    let mut pos = initial.len();
    for s in samples {
        file[pos..pos + s.payload.len()].copy_from_slice(&s.payload);
        pos += s.payload.len();
    }
    Ok(file)
}

/// Theora → OggS 打包（三头 + 数据包同一逻辑流；CRC 由 `ogg` 轮子算，
/// 与固件手写拆包同算法，roundtrip 测试锁死）。
pub(crate) fn mux_ogg_theora(headers: &[Vec<u8>], packets: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    use ogg::{PacketWriteEndInfo, PacketWriter};
    if headers.len() != 3 || packets.is_empty() {
        return Err(String::from("ogg: need 3 headers + data"));
    }
    let mut out = Vec::new();
    let mut w = PacketWriter::new(&mut out);
    let serial: u32 = 0x1951_0715;
    for (i, h) in headers.iter().enumerate() {
        w.write_packet(
            h.clone(),
            serial,
            PacketWriteEndInfo::NormalPacket,
            i as u64,
        )
        .map_err(|e| format!("ogg header: {e}"))?;
    }
    for (i, p) in packets.iter().enumerate() {
        let last = i + 1 == packets.len();
        w.write_packet(
            p.clone(),
            serial,
            if last {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            },
            // theora granule：帧序号单调递增（固件拆包不消费，仅保单调）。
            (headers.len() + i) as u64,
        )
        .map_err(|e| format!("ogg packet: {e}"))?;
    }
    drop(w);
    Ok(out)
}

#[cfg(test)]
mod transcode_tests {
    use super::*;

    #[test]
    fn target_output_paths_match_firmware_layout() {
        // avc/hevc/vp9 同名 video.mp4（播哪个由 [play] format 开关决定）。
        for (t, path) in [
            (TranscodeTarget::Avc, "1080p/video.mp4"),
            (TranscodeTarget::Hevc, "1080p/video.mp4"),
            (TranscodeTarget::Vp9, "1080p/video.mp4"),
            (TranscodeTarget::H263, "1080p/video.h263"),
            (TranscodeTarget::Mpeg2v, "1080p/video.m2v"),
            (TranscodeTarget::Theora, "1080p/video.ogv"),
            (TranscodeTarget::Qois, "1080p/video.qois"),
        ] {
            assert_eq!(t.out_path(), path);
        }
    }

    fn two_frames() -> Vec<Yuv420> {
        vec![
            Yuv420 {
                w: 64,
                h: 48,
                y: vec![16u8; 64 * 48],
                u: vec![128u8; 32 * 24],
                v: vec![128u8; 32 * 24],
            },
            Yuv420 {
                w: 64,
                h: 48,
                y: vec![235u8; 64 * 48],
                u: vec![128u8; 32 * 24],
                v: vec![128u8; 32 * 24],
            },
        ]
    }

    /// 解回 MP4：样片数 + 关键帧数 + 首样片 entry 種別（固件 demux 同 parser）。
    fn demux_check(mp4: &[u8], want_entry: &str) -> (usize, usize) {
        use shiguredo_mp4::boxes::SampleEntry;
        use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
        assert_eq!(&mp4[4..8], b"ftyp");
        let mut dx = Mp4FileDemuxer::new();
        dx.handle_input(Input {
            position: 0,
            data: mp4,
        });
        let mut n = 0;
        let mut keys = 0;
        let mut entry = String::new();
        loop {
            let s = dx.next_sample().expect("demux").clone();
            let Some(s) = s else { break };
            if !matches!(s.track.kind, shiguredo_mp4::TrackKind::Video) {
                continue;
            }
            n += 1;
            if s.keyframe {
                keys += 1;
            }
            if entry.is_empty() {
                entry = match s.sample_entry {
                    Some(SampleEntry::Avc1(_)) => "avc1",
                    Some(SampleEntry::Hvc1(_)) => "hvc1",
                    Some(SampleEntry::Hev1(_)) => "hev1",
                    Some(SampleEntry::Vp09(_)) => "vp09",
                    _ => "other",
                }
                .to_string();
            }
        }
        assert_eq!(entry, want_entry);
        (n, keys)
    }

    #[test]
    fn mp4_avc_roundtrip() {
        // less-avc 编 → avc1/avcC 打包 → 同轮子解回。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, keys) = demux_check(&mp4, "avc1");
        assert_eq!(n, 2);
        assert!(keys >= 1);
    }

    #[test]
    fn mp4_hevc_roundtrip() {
        // oxideav-h265 编 → hvc1/hvcC 打包 → 同轮子解回。
        let st = encode_hevc(&two_frames()).expect("hevc");
        let entry = hevc_entry(&st.vps, &st.sps, &st.pps, 64, 48, 30);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, keys) = demux_check(&mp4, "hvc1");
        assert_eq!(n, st.samples.len());
        assert!(keys >= 1);
    }

    #[test]
    fn mp4_vp9_roundtrip() {
        // rusty_vp9 编 → vp09/vpcC 打包（裸帧样片）→ 同轮子解回。
        let pkts = encode_vp9(&two_frames()).expect("vp9");
        let entry = vp9_entry(64, 48);
        let samples: Vec<Mp4OutSample> = pkts
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: x.data.clone(),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, _keys) = demux_check(&mp4, "vp09");
        assert_eq!(n, pkts.len());
    }

    #[test]
    fn probe_reports_mp4_track() {
        // 上面 avc 产出的 mp4 再探：轨种/几何/样片数对得上。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let info = probe_mp4(&mp4).expect("probe");
        assert_eq!(info.codec, "avc");
        assert_eq!((info.w, info.h), (64, 48));
        assert_eq!(info.n, 2);
    }

    /// 端到端冒烟：合成小 mp4 落盘 → 走完整 `run_transcode_job`（解复用→
    /// 流式解码→缩放→编码→mux→落盘）→ 产物可被同轮子 demux 回读。
    /// 覆盖生产路径的编排逻辑（进度/阶段/收尾），编解码正确性由各单项
    /// roundtrip 锁死。
    #[test]
    fn run_job_end_to_end_avc() {
        use std::sync::atomic::AtomicBool;
        let dir = std::env::temp_dir().join(format!("uefi-player-tc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.mp4");
        let out = dir.join("out").join("video.mp4");
        // 源：2 帧 avc → mp4（与固件 demux 同 parser 可读）。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        std::fs::write(&src, &mp4).expect("write src");
        // 整管线（阶段/进度回调只计数，不卡线程）。
        let mut stages = 0;
        let mut last = (0usize, 0usize);
        let cancel = AtomicBool::new(false);
        let r = run_transcode_job(
            &src.display().to_string(),
            TranscodeTarget::Avc,
            &out.display().to_string(),
            &cancel,
            |_| {
                stages += 1;
            },
            |d, t| {
                last = (d, t);
            },
        );
        let out_info = r.expect("run_transcode_job");
        assert!(stages >= 2, "want decode+encode/mux stages");
        assert_eq!(last.0, last.1);
        assert!(last.0 >= 2, "want frame-level progress, got {last:?}");
        let produced = std::fs::read(&out_info.path).expect("read output");
        assert!(produced.len() > 1024, "output too small");
        // 产物回读：2 样片 avc1。
        let (n, keys) = demux_check(&produced, "avc1");
        assert_eq!(n, 2);
        assert!(keys >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
