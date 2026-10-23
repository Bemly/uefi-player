//! 转码页：片源探针（选源时同步快探，不解码；拆自 `transcode.rs`，行为零改）。

use super::*;

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
    if lower.ends_with(".raws") {
        let len = std::fs::metadata(path)
            .map_err(|e| format!("stat: {e}"))?
            .len();
        let frame = 1920 * 1080 * 4;
        if len == 0 || len % frame as u64 != 0 {
            return Err(format!("raws: want N*1920x1080 BGRA, got {len} bytes"));
        }
        let n = (len / frame as u64) as usize;
        return Ok(ProbeInfo {
            container: "raws",
            codec: "bgra",
            w: 1920,
            h: 1080,
            fps: 30,
            n,
            dur_s: (n as u64 + 29) / 30,
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
pub(crate) fn probe_mp4(data: &[u8]) -> Result<ProbeInfo, String> {
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
