//! 转码页：MP4 整轨解复用 + 关键帧分组（拆自 `transcode.rs`，守单文件 ≤1000 行，行为零改）。

use super::*;

/// MP4 轨参数（首样片 entry 取参；SPS/PPS 以 Annex-B 前缀存）。
pub(crate) struct TrackMeta {
    id: u32,
    timescale: f64,
    length_size: usize,
    annexb_prefix: Vec<u8>,
}
/// 压缩样片（分组后解码；dur 只用于 fps 估算；pts 为显示时间戳，
/// DTS(`timestamp`) + `composition_time_offset`(ctts，无则0)，i128 防溢出。
/// mac VT 同步解码按解码序吐帧（B 帧即乱序），转码 VT 路按此排回显示序，
/// Baseline 无 B 帧时恒有序，排序即 no-op）。
pub(crate) struct RawSample {
    pub(crate) dur: f64,
    pub(crate) keyframe: bool,
    pub(crate) pts: i128,
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
pub(crate) fn demux_groups(
    data: &[u8],
    want: MediaFormat,
    cancel: Option<&AtomicBool>,
) -> Result<DemuxedTrack, String> {
    use shiguredo_mp4::boxes::SampleEntry;
    use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
    use shiguredo_mp4::TrackKind;
    let mut dx = Mp4FileDemuxer::new();
    dx.handle_input(Input { position: 0, data });
    let mut meta: Option<TrackMeta> = None;
    let mut kinds: Vec<(u32, MediaFormat)> = Vec::new();
    let mut raws: Vec<RawSample> = Vec::new();
    let mut seen = 0usize;
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
            // PTS = DTS + ctts（无 ctts 表即 DTS；B 帧必带 ctts 否则 MP4 本身无法显示）。
            pts: s.timestamp as i128 + s.composition_time_offset.unwrap_or(0) as i128,
            payload: data[s.data_offset as usize..end].to_vec(),
        });
        seen += 1;
        if seen % 32 == 0 {
            if let Some(c) = cancel {
                if c.load(Ordering::Relaxed) {
                    return Err(String::from("cancelled"));
                }
            }
        }
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
