//! 转码页：后台转码 job（编码累积/缩放/raw 流式/主流程；拆自 `transcode.rs`，行为零改）。

use super::*;

/// 编码累积器（流式：逐段 push，最后 finish 一次性包装；裸帧不过段）。
#[derive(Default)]
pub(crate) struct EncodeAcc {
    avc: Option<AvcStream>,
    hevc: Option<HevcStream>,
    vp9: Vec<Vp9PacketData>,
    theora_headers: Vec<Vec<u8>>,
    theora_packets: Vec<Vec<u8>>,
    bare: Vec<u8>,
    qois: Vec<u8>,
    // `Raw` 序列 BGRA 帧（单测/小片段用；正式 job 走流式直写，不经此累积，免大内存）。
    raw: Vec<Vec<u8>>,
}

impl EncodeAcc {
    /// 编码一段（一段即一次会话，段首恒关键帧；调用方按分段设置批量喂）。
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
            TranscodeTarget::Raw => {
                for f in g {
                    let rgba = yuv420_to_rgba(f);
                    let mut bgra = Vec::with_capacity(rgba.px.len());
                    for px in rgba.px.chunks_exact(4) {
                        bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                    }
                    self.raw.push(bgra);
                }
            }
        }
        Ok(())
    }

    fn finish(self, target: TranscodeTarget, w: u32, h: u32, fps: u32) -> Result<Vec<u8>, String> {
        match target {
            TranscodeTarget::Avc => {
                let s = self.avc.ok_or_else(|| String::from("avc: no stream"))?;
                let entry = avc_entry(&s.sps, &s.pps, w, h);
                let samples: Vec<Mp4OutSample> = s
                    .samples
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: length_prefixed_nals(&x.nalus),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, w, h, fps)
            }
            TranscodeTarget::Hevc => {
                let s = self.hevc.ok_or_else(|| String::from("hevc: no stream"))?;
                let entry = hevc_entry(&s.vps, &s.sps, &s.pps, w, h, fps);
                let samples: Vec<Mp4OutSample> = s
                    .samples
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: length_prefixed_nals(&x.nalus),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, w, h, fps)
            }
            TranscodeTarget::Vp9 => {
                let entry = vp9_entry(w, h);
                let samples: Vec<Mp4OutSample> = self
                    .vp9
                    .iter()
                    .map(|x| Mp4OutSample {
                        keyframe: x.keyframe,
                        payload: x.data.clone(),
                    })
                    .collect();
                mux_mp4_video(entry, &samples, w, h, fps)
            }
            TranscodeTarget::Theora => mux_ogg_theora(&self.theora_headers, &self.theora_packets),
            TranscodeTarget::H263 | TranscodeTarget::Mpeg2v => Ok(self.bare),
            TranscodeTarget::Qois => Ok(self.qois),
            // 单文件口径（BGRA 帧首尾相接）；正式 job 走流式追加，不经此累积。
            TranscodeTarget::Raw => Ok(self.raw.concat()),
        }
    }
}

/// 供片缩放：H263 按解码几何取内接标准格式 + 黑边保比（编码器硬门，
/// 见 `codecs::h263_letterbox`，无视分辨率选项）；其余目标按分辨率选项缩放，
/// Source 照源几何直通（固件侧 `display.fit` 保比缩放）。
pub(crate) fn scale_for(f: &Yuv420, target: TranscodeTarget, wh: Option<(u32, u32)>) -> Yuv420 {
    if target == TranscodeTarget::H263 {
        crate::codecs::h263_letterbox(f)
    } else if target == TranscodeTarget::Raw {
        // raw 序列定死 1920x1080 BGRA（固件 `from_raw_dir` 按帧字节定几何，无视分辨率选项）。
        crate::codecs::scale_yuv420(f, OUT_W, OUT_H)
    } else if let Some((w, h)) = wh {
        scale_yuv420(f, w, h)
    } else {
        f.clone()
    }
}

/// 帧率重采样（流式逐输入帧裁决输出份数；纯规则，可单测）。
/// 输入累计 in_seen（含当前帧）映射到输出累计：
/// out_seen = in_seen * out_fps / in_fps，下取整；
/// 本帧份数 = out_seen(后) - out_seen(前)。降采样自然丢帧，
/// 升采样自然补帧（调用方 clone），等帧率恒 1 份，总时长不变。
pub(crate) struct FpsPick {
    in_fps: u64,
    out_fps: u64,
    seen: u64,
}

impl FpsPick {
    pub(crate) fn new(in_fps: u32, out_fps: u32) -> Self {
        Self {
            in_fps: in_fps.max(1) as u64,
            out_fps: out_fps.max(1) as u64,
            seen: 0,
        }
    }

    pub(crate) fn copies_for_one_input(&mut self) -> usize {
        let before = self.seen * self.out_fps / self.in_fps;
        self.seen += 1;
        let after = self.seen * self.out_fps / self.in_fps;
        (after - before) as usize
    }
}

/// RGBA（`yuv420_to_rgba` 口径）→ BGRA（固件 `BltPixel` 内存布局）。
pub(crate) fn rgba_to_bgra(px: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len());
    for p in px.chunks_exact(4) {
        out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    out
}

///  单文件帧追加（流式不经  累积，免大内存；
/// BGRA 帧首尾相接无头，固件  按  整切）。
pub(crate) fn append_raw_frame(out: &mut std::fs::File, bgra: &[u8]) -> Result<usize, String> {
    use std::io::Write;
    out.write_all(bgra).map_err(|e| format!("write raw: {e}"))?;
    Ok(bgra.len())
}

/// `Raw` 序列 job（mp4/静帧 → 单文件 `1080p/video.raws`，BGRA 帧首尾相接流式追加）。
/// 与 `run_transcode_job` 同解码链（avc 硬解优先 + 同顺序 demux），
/// 缩放恒 `scale_for(Raw)` = 1920x1080（无视 `res_wh`/`seg` 之 `res`/`seg`）；
/// 整文件即序列（`frame_bytes` 整切，无头，固件 `from_raws_bytes` 同口径）。
pub(crate) fn run_raw_job(
    source: &str,
    out_path: &str,
    cancel: &AtomicBool,
    mut stage: impl FnMut(JobStage),
    mut progress: impl FnMut(usize, usize),
) -> Result<JobOut, String> {
    use std::io::Write;
    use std::path::Path;
    let out_p = Path::new(out_path);
    if let Some(parent) = out_p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
    }
    let mut out = std::fs::File::create(out_p).map_err(|e| format!("create: {e}"))?;
    let bytes = std::fs::read(source).map_err(|e| format!("read source: {e}"))?;
    let lower = source.to_ascii_lowercase();
    stage(JobStage::Decode);
    let mut idx = 0usize;
    let mut total_bytes = 0usize;
    let mut emit = |rgba_w: u32, rgba_h: u32, px: &[u8]| -> Result<(), String> {
        let _ = (rgba_w, rgba_h);
        let bgra = rgba_to_bgra(px);
        out.write_all(&bgra)
            .map_err(|e| format!("write raw: {e}"))?;
        total_bytes += bgra.len();
        idx += 1;
        progress(idx, idx);
        Ok(())
    };
    if lower.ends_with(".mp4") {
        let mut demuxed = None;
        for want in [MediaFormat::Avc, MediaFormat::Hevc, MediaFormat::Vp9] {
            match demux_groups(&bytes, want, Some(cancel)) {
                Ok(d) if d.total > 0 => {
                    demuxed = Some((want, d));
                    break;
                }
                _ => continue,
            }
        }
        let (want, d) = demuxed.ok_or_else(|| String::from("mp4: no avc/hevc/vp9 track"))?;
        let mut dec = TrackDecoder::new(want);
        stage(JobStage::Encode);
        for g in &d.groups {
            if cancel.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
            for f in decode_group(&mut dec, &d, g)? {
                let s = scale_for(&f, TranscodeTarget::Raw, None);
                let rgba = yuv420_to_rgba(&s);
                emit(rgba.w, rgba.h, &rgba.px)?;
            }
        }
        for f in finish_decode(&mut dec)? {
            let s = scale_for(&f, TranscodeTarget::Raw, None);
            let rgba = yuv420_to_rgba(&s);
            emit(rgba.w, rgba.h, &rgba.px)?;
        }
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
    {
        let img = image::open(Path::new(source))
            .map_err(|e| format!("image decode: {e}"))?
            .to_rgba8();
        let (w, h) = (img.width(), img.height());
        let rgba = RgbaFrame {
            w,
            h,
            px: img.into_raw(),
        };
        let yuv = rgba_to_yuv420(&rgba);
        let s = scale_for(&yuv, TranscodeTarget::Raw, None);
        let out = yuv420_to_rgba(&s);
        stage(JobStage::Encode);
        emit(out.w, out.h, &out.px)?;
    } else if lower.ends_with(".raws") {
        let frame = 1920 * 1080 * 4;
        if bytes.is_empty() || bytes.len() % frame != 0 {
            return Err(format!(
                "raws: want N*1920x1080 BGRA, got {} bytes",
                bytes.len()
            ));
        }
        stage(JobStage::Encode);
        out.write_all(&bytes)
            .map_err(|e| format!("write raw: {e}"))?;
        total_bytes += bytes.len();
        idx += bytes.len() / frame;
        progress(idx, idx);
    } else if lower.ends_with(".raw") {
        if bytes.len() != 1920 * 1080 * 4 {
            return Err(format!(
                "raw: want 1920x1080 BGRA, got {} bytes",
                bytes.len()
            ));
        }
        stage(JobStage::Encode);
        out.write_all(&bytes)
            .map_err(|e| format!("write raw: {e}"))?;
        total_bytes += bytes.len();
        idx += 1;
        progress(idx, idx);
    } else {
        return Err(format!("unsupported source: {source}"));
    }
    if idx == 0 {
        return Err(String::from("mp4: no decodable frames"));
    }
    stage(JobStage::Mux);
    drop(out);
    Ok(JobOut {
        path: out_path.to_string(),
        bytes: total_bytes,
        hw_fallback: crate::codecs::take_hw_fallback_hit(),
    })
}

/// 后台转码主流程（worker 线程内跑；`stage` 报阶段，`progress` 报帧进度）。
/// 流式：demux 分组 → 逐组解码→缩放→编码→释放（裸帧不过组，内存封顶）。
/// `Raw` 例外走单文件流式追加（见 `run_raw_job`，不经 `EncodeAcc`）。
#[allow(clippy::too_many_lines)]
pub(crate) fn run_transcode_job(
    source: &str,
    target: TranscodeTarget,
    out_path: &str,
    cancel: &AtomicBool,
    mut stage: impl FnMut(JobStage),
    mut progress: impl FnMut(usize, usize),
    res_wh: Option<(u32, u32)>,
    out_fps: Option<u32>,
    seg: usize,
) -> Result<JobOut, String> {
    // 本 job 的回退标记在本线程清零；收尾时读取（转码 worker 线独占，无串扰）。
    crate::codecs::take_hw_fallback_hit();
    // `Raw` 序列走单文件流式追加（不经 `EncodeAcc`，免大内存）。
    if target == TranscodeTarget::Raw {
        return run_raw_job(source, out_path, cancel, stage, progress);
    }
    let bytes = std::fs::read(source).map_err(|e| format!("read source: {e}"))?;
    let lower = source.to_ascii_lowercase();
    stage(JobStage::Decode);
    let mut acc = EncodeAcc::default();
    let mut done = 0usize;
    let mut encoding = false;
    let mut out_wh: Option<(u32, u32)> = res_wh;
    let fps: u32;
    if lower.ends_with(".mp4") {
        // 源轨按容器头选：首个有样片的 avc/hevc/vp9 轨（与固件同顺序）。
        let mut demuxed = None;
        for want in [MediaFormat::Avc, MediaFormat::Hevc, MediaFormat::Vp9] {
            match demux_groups(&bytes, want, Some(cancel)) {
                Ok(d) if d.total > 0 => {
                    demuxed = Some((want, d));
                    break;
                }
                _ => continue,
            }
        }
        let (want, d) = demuxed.ok_or_else(|| String::from("mp4: no avc/hevc/vp9 track"))?;
        let src_fps = d.fps;
        fps = out_fps.unwrap_or(src_fps);
        let total = if fps == src_fps {
            d.total
        } else {
            ((d.total as u64 * fps as u64 + src_fps as u64 / 2) / src_fps as u64) as usize
        };
        let mut pick = FpsPick::new(src_fps, fps);
        let mut pending: Vec<Yuv420> = Vec::new();
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
            for f in &frames {
                if cancel.load(Ordering::Relaxed) {
                    return Err(String::from("cancelled"));
                }
                let s = scale_for(f, target, res_wh);
                if out_wh.is_none() {
                    out_wh = Some((s.w, s.h));
                }
                for _ in 0..pick.copies_for_one_input() {
                    pending.push(s.clone());
                }
                while pending.len() >= seg {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(String::from("cancelled"));
                    }
                    let chunk: Vec<Yuv420> = pending.drain(..seg).collect();
                    acc.push(target, &chunk)?;
                    done += chunk.len();
                    progress(done, total);
                }
            }
        }
        // 尾部排空（HEVC flush；其余空）。
        let tail = finish_decode(&mut dec)?;
        if !tail.is_empty() {
            stage(JobStage::Encode);
            for f in &tail {
                if cancel.load(Ordering::Relaxed) {
                    return Err(String::from("cancelled"));
                }
                let s = scale_for(f, target, res_wh);
                if out_wh.is_none() {
                    out_wh = Some((s.w, s.h));
                }
                for _ in 0..pick.copies_for_one_input() {
                    pending.push(s.clone());
                }
                while pending.len() >= seg {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(String::from("cancelled"));
                    }
                    let chunk: Vec<Yuv420> = pending.drain(..seg).collect();
                    acc.push(target, &chunk)?;
                    done += chunk.len();
                    progress(done, total);
                }
            }
        }
        if !pending.is_empty() {
            if cancel.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
            acc.push(target, &pending)?;
            done += pending.len();
            progress(done, total);
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
        fps = out_fps.unwrap_or(30);
        let total = frames.len();
        stage(JobStage::Encode);
        for chunk in frames.chunks(seg.max(1)) {
            let scaled: Vec<Yuv420> = chunk.iter().map(|f| scale_for(f, target, res_wh)).collect();
            if out_wh.is_none() {
                if let Some(f0) = scaled.first() {
                    out_wh = Some((f0.w, f0.h));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(String::from("cancelled"));
            }
            acc.push(target, &scaled)?;
            done += chunk.len();
            progress(done, total);
        }
    }
    // 容器包装 + 落盘（几何取首个缩放帧回退 1080p，Source 无帧不可能到此）。
    stage(JobStage::Mux);
    let (ow, oh) = out_wh.unwrap_or((OUT_W, OUT_H));
    // 目标格式几何门（跟源 4K 直通也可能超规，显式档 UI 侧已拦一道）：
    // mpeg2v 只到 MP@HL（1920x1152），超了直接 Err，不断 mux。
    if target == TranscodeTarget::Mpeg2v && (ow > 1920 || oh > 1152) {
        return Err(String::from(
            "mpeg2v: output WxH exceeds MP@HL (1920x1152), pick 1080p or lower",
        ));
    }
    let bytes = acc.finish(target, ow, oh, fps)?;

    if let Some(parent) = std::path::Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
    }
    std::fs::write(out_path, &bytes).map_err(|e| format!("write: {e}"))?;
    Ok(JobOut {
        path: out_path.to_string(),
        bytes: bytes.len(),
        hw_fallback: crate::codecs::take_hw_fallback_hit(),
    })
}
