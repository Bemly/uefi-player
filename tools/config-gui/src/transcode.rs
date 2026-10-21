//! 转换编码页：片源 → 固件可播格式，落在图库（`player.toml` 同目录）。
//!
//! 流程：选源（图库文件/`rfd`/示例片）→ 取[start, start+take) → 解码 →
//! 缩到 1920x1080 → 七目标编码 → 固件布局落盘（`1080p/video.*`）。
//! 编码在 worker 线程跑（`mpsc` 进度回传），UI 只轮询不阻塞；取消用
//! 原子旗标按组检查。内存纪律见 `codecs.rs` 头注释（关键帧分组+组内 30 帧段）。
//! 件库约束同其它页（GlassButton/GlassSlider/留白，无 TextEdit/ComboBox；
//! 路径显示只用 monospace 文本）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

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
    /// 图库相对路径（固件布局）。
    pub(crate) fn out_path(self) -> &'static str {
        match self {
            Self::Avc => "1080p/video.mp4",
            Self::Hevc => "1080p/video.hevc",
            Self::Vp9 => "1080p/video.ivf",
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
    Finished(Result<JobOut, String>),
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

/// 转码状态（`App` 持有；`start_s`/`take_s` 是滑杆镜像，`f32`）。
pub(crate) struct TranscodeState {
    pub(crate) source: String,
    pub(crate) start_s: f32,
    pub(crate) take_s: f32,
    pub(crate) target: TranscodeTarget,
    pub(crate) status: String,
    pub(crate) job: Option<TranscodeJob>,
    pub(crate) progress: Option<(usize, usize)>,
}

impl TranscodeState {
    pub(crate) fn new() -> Self {
        Self {
            source: String::new(),
            start_s: 0.0,
            take_s: 40.0,
            target: TranscodeTarget::Avc,
            status: String::new(),
            job: None,
            progress: None,
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.job.is_some()
    }
}

/// 组封顶（关键帧自然分组 + 硬切；60 帧×8MB ≈ 480MB 瞬时上限，
/// 超了硬切 + 状态栏警告，不断转码）。
pub(crate) const GROUP_CAP: usize = 60;
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
/// 窗内样片（压缩态；分组后解码；时长只用于窗内 fps 估算）。
struct RawSample {
    dur: f64,
    keyframe: bool,
    payload: Vec<u8>,
}

/// MP4 源解码（want 三轨 + 时间窗；返回 YUV420 帧 + 实测 fps）。
pub(crate) fn decode_mp4_range(
    data: &[u8],
    want: MediaFormat,
    start_s: f64,
    take_s: f64,
) -> Result<DecodedClip, String> {
    use shiguredo_mp4::boxes::SampleEntry;
    use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
    use shiguredo_mp4::TrackKind;
    let mut dx = Mp4FileDemuxer::new();
    dx.handle_input(Input { position: 0, data });
    let mut meta: Option<TrackMeta> = None;
    let mut raws: Vec<RawSample> = Vec::new();
    loop {
        let s = dx.next_sample().map_err(|e| format!("mp4 demux: {e:?}"))?;
        let Some(s) = s else { break };
        if s.track.kind != TrackKind::Video {
            continue;
        }
        let kind = match s.sample_entry {
            Some(SampleEntry::Avc1(_)) => MediaFormat::Avc,
            Some(SampleEntry::Hev1(_)) | Some(SampleEntry::Hvc1(_)) => MediaFormat::Hevc,
            Some(SampleEntry::Vp09(_)) | Some(SampleEntry::Vp08(_)) => MediaFormat::Vp9,
            _ => continue,
        };
        if kind != want {
            continue;
        }
        let ts = s.timestamp as f64 / s.track.timescale.get() as f64;
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
        if ts < start_s || ts >= start_s + take_s {
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
        return Err(String::from("mp4: no samples in range"));
    }
    // fps：窗内平均样片间隔。
    let total_dur: f64 = raws.iter().map(|r| r.dur).sum();
    let fps = if total_dur > 0.0 {
        (raws.len() as f64 / total_dur).round().clamp(1.0, 120.0) as u32
    } else {
        30
    };
    // 关键帧分组（硬切封顶，超了后面警告）。
    let mut groups: Vec<Vec<RawSample>> = Vec::new();
    let mut capped = false;
    for r in raws {
        let fresh = r.keyframe
            || groups
                .last()
                .map(|g: &Vec<RawSample>| g.len() >= GROUP_CAP)
                .unwrap_or(true);
        if fresh {
            if groups
                .last()
                .map(|g: &Vec<RawSample>| g.len() >= GROUP_CAP)
                .unwrap_or(false)
            {
                capped = true;
            }
            groups.push(Vec::new());
        }
        if groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(r);
    }
    // 逐组解码（YUV420 紧凑帧；组播完即转段，不驻留）。
    let mut frames: Vec<Yuv420> = Vec::new();
    let (mut gw, mut gh) = (0u32, 0u32);
    for g in &groups {
        let mut gf = decode_group(&m, g, want)?;
        if gw == 0 {
            gw = gf.w;
            gh = gf.h;
        }
        if gf.w != gw || gf.h != gh {
            continue;
        }
        frames.append(&mut gf.frames);
    }
    if frames.is_empty() {
        return Err(String::from("mp4: no decodable frames"));
    }
    if capped {
        // 调用方把警告进 status（此处只保行为）。
    }
    Ok(DecodedClip {
        w: gw,
        h: gh,
        fps,
        frames,
    })
}

/// 单组解码（SPS/PPS 前缀 + 整组 Annex-B/裸帧；形状与固件一致）。
fn decode_group(m: &TrackMeta, g: &[RawSample], want: MediaFormat) -> Result<DecodedClip, String> {
    match want {
        MediaFormat::Avc => {
            let mut annexb = m.annexb_prefix.clone();
            for r in g {
                annexb.extend(length_prefixed_to_annexb(&r.payload, m.length_size));
            }
            let decoded = rusty_h264_decoder::Decoder::new()
                .decode_stream(&annexb)
                .map_err(|e| format!("avc decode: {e:?}"))?;
            let mut frames = Vec::new();
            let (mut w, mut h) = (0u32, 0u32);
            for f in &decoded {
                let planes = f.as_planes();
                if !planes.is_tight() {
                    continue;
                }
                if w == 0 {
                    w = f.width as u32;
                    h = f.height as u32;
                }
                if f.width as u32 != w || f.height as u32 != h {
                    continue;
                }
                frames.push(Yuv420 {
                    w: f.width as u32,
                    h: f.height as u32,
                    y: planes.y.to_vec(),
                    u: planes.u.to_vec(),
                    v: planes.v.to_vec(),
                });
            }
            Ok(DecodedClip {
                w,
                h,
                fps: 30,
                frames,
            })
        }
        MediaFormat::Hevc => {
            let mut annexb = m.annexb_prefix.clone();
            for r in g {
                annexb.extend(length_prefixed_to_annexb(&r.payload, m.length_size));
            }
            let mut dec = rusty_h265::Decoder::new();
            dec.push_annexb(&annexb, None)
                .map_err(|e| format!("hevc push: {e:?}"))?;
            dec.flush();
            let mut frames = Vec::new();
            let (mut w, mut h) = (0u32, 0u32);
            loop {
                let f = match dec.next_frame() {
                    Ok(f) => f,
                    Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
                    Err(_) => continue,
                };
                if f.bit_depth() != 8 {
                    continue;
                }
                if w == 0 {
                    w = f.width as u32;
                    h = f.height as u32;
                }
                if f.width as u32 != w || f.height as u32 != h {
                    continue;
                }
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
                frames.push(Yuv420 {
                    w,
                    h,
                    y: ypl.to_vec(),
                    u: upl.to_vec(),
                    v: vpl.to_vec(),
                });
            }
            Ok(DecodedClip {
                w,
                h,
                fps: 30,
                frames,
            })
        }
        MediaFormat::Vp9 => {
            let mut dec = vp9dec::Decoder::new();
            let mut frames = Vec::new();
            let (mut w, mut h) = (0u32, 0u32);
            for r in g {
                let decoded = dec
                    .decode_frame(&r.payload)
                    .map_err(|e| format!("vp9 decode: {e:?}"))?;
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
                    if w == 0 {
                        w = f.width;
                        h = f.height;
                    }
                    if f.width != w || f.height != h {
                        continue;
                    }
                    frames.push(Yuv420 {
                        w,
                        h,
                        y: y.clone(),
                        u: u.clone(),
                        v: v.clone(),
                    });
                }
            }
            Ok(DecodedClip {
                w,
                h,
                fps: 30,
                frames,
            })
        }
        _ => Err(String::from("mp4: unsupported track")),
    }
}

impl App {
    /// 转码页：源行 + 时间窗滑杆 + 目标行 + 输出行 + 编码按钮 + 状态。
    pub(crate) fn transcode_page(&mut self, ui: &mut eframe::egui::Ui) {
        use rust_i18n::t;
        // 先收 worker 进度（UI 线程只 try_recv，不阻塞）。
        self.poll_transcode_job();
        ui.heading(t!("transcode.heading"));
        ui.small(t!("transcode.hint"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(t!("transcode.source"));
            let shown = if self.transcode.source.is_empty() {
                t!("transcode.no_source").into_owned()
            } else {
                self.transcode.source.clone()
            };
            ui.monospace(shown);
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
                    self.transcode.status.clear();
                }
            }
            if egui_glass::GlassButton::new(t!("transcode.example"))
                .show(ui)
                .clicked()
            {
                self.transcode.source = String::from("/Users/bemly/Movies/input.mp4");
                self.transcode.status.clear();
            }
            if egui_glass::GlassButton::new(t!("transcode.from_gallery"))
                .show(ui)
                .clicked()
            {
                self.page = crate::app::Page::Gallery;
            }
        });
        ui.add_space(10.0);
        ui.label(t!("transcode.range"));
        let start_label = format!("start {}s", self.transcode.start_s as u32);
        egui_glass::GlassSlider::new(&mut self.transcode.start_s, 0.0..=600.0)
            .text(start_label)
            .step_by(1.0)
            .style(if self.dark {
                egui_glass::GlassStyle::dark()
            } else {
                egui_glass::GlassStyle::regular()
            })
            .show(ui);
        self.transcode.start_s = self.transcode.start_s.clamp(0.0, 600.0);
        let take_label = format!("take {}s", self.transcode.take_s as u32);
        egui_glass::GlassSlider::new(&mut self.transcode.take_s, 1.0..=120.0)
            .text(take_label)
            .step_by(1.0)
            .style(if self.dark {
                egui_glass::GlassStyle::dark()
            } else {
                egui_glass::GlassStyle::regular()
            })
            .show(ui);
        self.transcode.take_s = self.transcode.take_s.clamp(1.0, 120.0);
        ui.small(t!("transcode.range_hint"));
        ui.add_space(10.0);
        ui.label(t!("transcode.target"));
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
        ui.small(crate::text::transcode_target_desc(self.transcode.target));
        ui.add_space(10.0);
        let out_rel = self.transcode.target.out_path();
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(out_rel)
            .display()
            .to_string();
        ui.horizontal(|ui| {
            ui.label(t!("transcode.output"));
            ui.monospace(out_full.as_str());
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
                ui.label(format!("{done}/{total}"));
            }
        });
        if !self.transcode.status.is_empty() {
            ui.add_space(4.0);
            ui.small(self.transcode.status.as_str());
        }
    }

    /// worker 进度轮询 + 收尾（worker 线程落盘，UI 只报状态）。
    pub(crate) fn poll_transcode_job(&mut self) {
        let mut finished: Option<Result<JobOut, String>> = None;
        let mut progress = None;
        if let Some(job) = self.transcode.job.as_mut() {
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    JobMsg::Progress { done, total } => progress = Some((done, total)),
                    JobMsg::Finished(r) => {
                        finished = Some(r);
                        break;
                    }
                }
            }
        }
        if let Some(p) = progress {
            self.transcode.progress = Some(p);
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
        let source = self.transcode.source.clone();
        let out_rel = self.transcode.target.out_path().to_string();
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(&out_rel)
            .display()
            .to_string();
        let target = self.transcode.target;
        let start_s = self.transcode.start_s as f64;
        let take_s = self.transcode.take_s as f64;
        let (tx, rx): (Sender<JobMsg>, Receiver<JobMsg>) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let handle = std::thread::spawn(move || {
            let r = run_transcode_job(
                &source,
                start_s,
                take_s,
                target,
                &out_full,
                &cancel_w,
                |d, t| {
                    let _ = tx.send(JobMsg::Progress { done: d, total: t });
                },
            );
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

/// 后台转码主流程（worker 线程内跑；`progress` 回调报帧进度）。
pub(crate) fn run_transcode_job(
    source: &str,
    start_s: f64,
    take_s: f64,
    target: TranscodeTarget,
    out_path: &str,
    cancel: &AtomicBool,
    progress: impl Fn(usize, usize),
) -> Result<JobOut, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("read source: {e}"))?;
    let lower = source.to_ascii_lowercase();
    // 解码到 YUV420（源尺寸）+ fps。
    let clip = if lower.ends_with(".mp4") {
        // 源轨按容器头选：avc/hevc/vp9 首个视频轨（与固件 `demux_mp4_track` 同顺序）。
        decode_mp4_auto(&bytes, start_s, take_s)?
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
    {
        let img = image::open(std::path::Path::new(source))
            .map_err(|e| format!("image decode: {e}"))?
            .to_rgba8();
        let (w, h) = (img.width(), img.height());
        let px = img.into_raw();
        let rgba = RgbaFrame { w, h, px };
        DecodedClip {
            w,
            h,
            fps: 30,
            frames: vec![rgba_to_yuv420(&rgba)],
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
        let rgba = RgbaFrame {
            w: 1920,
            h: 1080,
            px,
        };
        DecodedClip {
            w: 1920,
            h: 1080,
            fps: 30,
            frames: vec![rgba_to_yuv420(&rgba)],
        }
    } else {
        return Err(format!("unsupported source: {source}"));
    };
    if clip.frames.is_empty() {
        return Err(String::from("no frames decoded"));
    }
    let fps = clip.fps;
    // 缩到输出几何（组是关键帧自然边界；组首恒关键帧，拼接恒合法）。
    let total = clip.frames.len();
    let mut done = 0usize;
    // 编码器按组重建；内存纪律见 codecs 头注释（组封顶 60）。
    let mut groups: Vec<Vec<Yuv420>> = vec![Vec::new()];
    for f in &clip.frames {
        if groups.last().map(|g| g.len() >= GROUP_CAP).unwrap_or(false) {
            groups.push(Vec::new());
        }
        groups
            .last_mut()
            .unwrap()
            .push(scale_yuv420(f, OUT_W, OUT_H));
    }
    drop(clip);
    let mut avc_stream: Option<AvcStream> = None;
    let mut vp9_packets: Vec<Vp9PacketData> = Vec::new();
    let mut theora_packets: Vec<Vec<u8>> = Vec::new();
    let mut theora_headers: Vec<Vec<u8>> = Vec::new();
    let mut bare: Vec<u8> = Vec::new();
    let mut qois: Vec<u8> = Vec::new();
    for g in &groups {
        if cancel.load(Ordering::Relaxed) {
            return Err(String::from("cancelled"));
        }
        match target {
            TranscodeTarget::Avc => {
                let s = encode_avc(g)?;
                if avc_stream.is_none() {
                    avc_stream = Some(AvcStream {
                        sps: s.sps.clone(),
                        pps: s.pps.clone(),
                        samples: Vec::new(),
                    });
                }
                avc_stream.as_mut().unwrap().samples.extend(s.samples);
            }
            TranscodeTarget::Vp9 => vp9_packets.extend(encode_vp9(g)?),
            TranscodeTarget::Theora => {
                let t = encode_theora(g)?;
                if theora_headers.is_empty() {
                    theora_headers = t.header_packets;
                }
                theora_packets.extend(t.data_packets);
            }
            TranscodeTarget::Hevc => bare.extend(encode_hevc(g)?),
            TranscodeTarget::H263 => bare.extend(encode_h263(g)?),
            TranscodeTarget::Mpeg2v => bare.extend(encode_mpeg2v(g)?),
            TranscodeTarget::Qois => {
                for f in g {
                    let rgba = yuv420_to_rgba(f);
                    let q = qoi::encode_to_vec(&rgba.px, rgba.w, rgba.h)
                        .map_err(|e| format!("qoi: {e:?}"))?;
                    qois.extend_from_slice(&(q.len() as u32).to_le_bytes());
                    qois.extend_from_slice(&q);
                }
            }
        }
        done += g.len();
        progress(done, total);
    }
    // 容器包装 + 落盘。
    let bytes = match target {
        TranscodeTarget::Avc => {
            let s = avc_stream.ok_or_else(|| String::from("avc: no stream"))?;
            mux_mp4_avc(&s, OUT_W, OUT_H, fps)?
        }
        TranscodeTarget::Vp9 => {
            let raws: Vec<Vec<u8>> = vp9_packets.into_iter().map(|p| p.data).collect();
            mux_ivf(&raws, OUT_W as u16, OUT_H as u16)
        }
        TranscodeTarget::Theora => mux_ogg_theora(&theora_headers, &theora_packets)?,
        TranscodeTarget::Hevc | TranscodeTarget::H263 | TranscodeTarget::Mpeg2v => bare,
        TranscodeTarget::Qois => qois,
    };
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

/// MP4 自动选轨解码（avc → hevc → vp9 顺序，首个命中轨；与固件同顺序）。
fn decode_mp4_auto(data: &[u8], start_s: f64, take_s: f64) -> Result<DecodedClip, String> {
    for want in [MediaFormat::Avc, MediaFormat::Hevc, MediaFormat::Vp9] {
        match decode_mp4_range(data, want, start_s, take_s) {
            Ok(clip) => return Ok(clip),
            Err(_) => continue,
        }
    }
    Err(String::from("mp4: no avc/hevc/vp9 track in range"))
}

/// AVC → MP4 打包（shiguredo 同轮子；`avc1/avcC` + 长度前缀样片）。
pub(crate) fn mux_mp4_avc(stream: &AvcStream, w: u32, h: u32, fps: u32) -> Result<Vec<u8>, String> {
    use shiguredo_mp4::boxes::{Avc1Box, AvccBox, SampleEntry, VisualSampleEntryFields};
    use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};
    use shiguredo_mp4::{TrackKind, Uint};
    use std::num::NonZeroU32;
    if stream.samples.is_empty() {
        return Err(String::from("mp4: no samples"));
    }
    let sps = stream.sps.clone();
    let pps = stream.pps.clone();
    let entry = SampleEntry::Avc1(Avc1Box {
        visual: VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width: w as u16,
            height: h as u16,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        },
        avcc_box: AvccBox {
            avc_profile_indication: sps.get(1).copied().unwrap_or(66),
            profile_compatibility: sps.get(2).copied().unwrap_or(0),
            avc_level_indication: sps.get(3).copied().unwrap_or(30),
            length_size_minus_one: Uint::new(3),
            sps_list: vec![sps],
            pps_list: vec![pps],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext_list: vec![],
        },
        unknown_boxes: vec![],
    });
    let mut muxer = Mp4FileMuxer::new().map_err(|e| format!("mux init: {e:?}"))?;
    let timescale = NonZeroU32::new(90000).unwrap();
    let dur = 90000u32 / fps.max(1);
    // 样片负载先拼好（长度前缀），offset 按顺序排。
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for s in &stream.samples {
        let mut pl = Vec::new();
        for nalu in &s.nalus {
            pl.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
            pl.extend_from_slice(nalu);
        }
        payloads.push(pl);
    }
    let mut data_offset = muxer.initial_boxes_bytes().len() as u64;
    for (i, (s, pl)) in stream.samples.iter().zip(payloads.iter()).enumerate() {
        muxer
            .append_sample(&Sample {
                track_kind: TrackKind::Video,
                sample_entry: if i == 0 { Some(entry.clone()) } else { None },
                keyframe: s.keyframe,
                timescale,
                duration: dur,
                composition_time_offset: None,
                data_offset,
                data_size: pl.len(),
            })
            .map_err(|e| format!("mux append: {e:?}"))?;
        data_offset += pl.len() as u64;
    }
    let initial = muxer.initial_boxes_bytes().to_vec();
    let finalized = muxer
        .finalize()
        .map_err(|e| format!("mux finalize: {e:?}"))?;
    // 组装：head（pairs 改写）+ mdat 头（initial 尾部 16B，pairs 改写终值）+
    // 样片负载（紧跟 initial）+ moov（pairs 偏移）。
    let mut max_end = initial.len() + payloads.iter().map(|p| p.len()).sum::<usize>();
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
    for pl in &payloads {
        file[pos..pos + pl.len()].copy_from_slice(pl);
        pos += pl.len();
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
        for (t, path) in [
            (TranscodeTarget::Avc, "1080p/video.mp4"),
            (TranscodeTarget::Hevc, "1080p/video.hevc"),
            (TranscodeTarget::Vp9, "1080p/video.ivf"),
            (TranscodeTarget::H263, "1080p/video.h263"),
            (TranscodeTarget::Mpeg2v, "1080p/video.m2v"),
            (TranscodeTarget::Theora, "1080p/video.ogv"),
            (TranscodeTarget::Qois, "1080p/video.qois"),
        ] {
            assert_eq!(t.out_path(), path);
        }
    }

    #[test]
    fn mp4_roundtrip_through_same_muxer() {
        // 编码→打包→同轮子解回：固件 demux 就是这个 parser，零口径差。
        use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
        let frames = vec![
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
        ];
        let stream = encode_avc(&frames).expect("avc");
        let mp4 = mux_mp4_avc(&stream, 64, 48, 30).expect("mux");
        assert!(&mp4[4..8] == b"ftyp");
        let mut dx = Mp4FileDemuxer::new();
        dx.handle_input(Input {
            position: 0,
            data: &mp4,
        });
        let mut n = 0;
        loop {
            let s = dx.next_sample().expect("demux").clone();
            let Some(s) = s else { break };
            if matches!(s.track.kind, shiguredo_mp4::TrackKind::Video) {
                n += 1;
            }
        }
        assert_eq!(n, 2);
    }
}
