//! 转码页：单轨流式解码（硬解/软解分派 + 收尾排空；拆自 `transcode.rs`，行为零改）。

use super::*;

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
    /// mac VideoToolbox AVC 会话（首组建，跨组复用；全 profile + PTS 保序，建会话失败即 CPU）。
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
pub(crate) fn pack_rows(src: &[u8], stride: usize, w: usize, h: usize) -> Option<Vec<u8>> {
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

/// mac VideoToolbox AVC 硬解（Nv12 回读 → 拆平面；全 profile，
/// B 帧按 PTS 排回显示序；建会话失败返回 None 走 CPU）。
///
/// 实测（2026-10-22 scratch 探针，High 样片 10 帧含 1 B 帧）：VT 同步
/// `decode()` 1:1 即时吐帧（零 `None`、像素与 rusty CPU 逐字节一致），
/// 但吐的是解码序（`vt[3]=cpu[4], vt[4]=cpu[3]`）。故按输入样片 PTS
/// （`RawSample.pts`，DTS + ctts）稳定排序后再 `push_frame`；Baseline
/// 无 B 帧恒有序，排序即 no-op。组按 IDR 切分，组内排序 + 组拼接即全轨
/// 显示序（开 GOP 前导 B 仍落同组内，同样排对）。
#[cfg(target_os = "macos")]
impl TrackDecoder {
    fn decode_avc_vt(&mut self, d: &DemuxedTrack, g: &[RawSample]) -> Option<Vec<Yuv420>> {
        use shiguredo_video_toolbox::{Decoder, DecoderCodec, DecoderConfig, PixelFormat};
        if self.vt_avc_failed {
            return None;
        }
        if self.vt_avc.is_none() {
            let units = crate::codecs::split_annexb(&d.annexb_prefix);
            if units.len() < 2 || units[0].is_empty() || units[0][0] & 0x1F != 7 {
                crate::codecs::note_hw_fallback();
                self.vt_avc_failed = true;
                return None;
            }
            // 会话内已拷贝参数集（video_bg 同式）；进程级常驻，不重复泄漏之忧
            // （转码 job 级 decoder 随 job 一生只建一次）。
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
                    crate::codecs::note_hw_fallback();
                    self.vt_avc_failed = true;
                    return None;
                }
            }
        }
        // (pts, w, h, y, uv)：VT 按解码序吐帧，攒完按 pts 排回显示序。
        let mut got: Vec<(i128, usize, usize, Vec<u8>, Vec<u8>)> = Vec::new();
        for r in g {
            // 借用内转 owned（帧借 decoder），借用外再攒。
            let one: Option<(usize, usize, Vec<u8>, Vec<u8>)> = match self.vt_avc.as_mut() {
                Some(dec) => match dec.decode(&r.payload) {
                    Ok(Some(f)) => {
                        if let shiguredo_video_toolbox::DecodedFrame::Nv12(nv) = f {
                            let (w, h) = (nv.width() as usize, nv.height() as usize);
                            if w == 0 || h == 0 {
                                None
                            } else {
                                match (
                                    pack_rows(nv.y_plane(), nv.y_stride(), w, h),
                                    pack_rows(nv.uv_plane(), nv.uv_stride(), w, h / 2),
                                ) {
                                    (Some(y), Some(uv)) => Some((w, h, y, uv)),
                                    _ => None,
                                }
                            }
                        } else {
                            None
                        }
                    }
                    // 坏帧/重排等待即跳过该输入（旧路同纪律；实测 High
                    // 10 帧零 None此处恒走 Some）。
                    Ok(None) | Err(_) => None,
                },
                None => break,
            };
            if let Some((w, h, y, uv)) = one {
                got.push((r.pts, w, h, y, uv));
            }
        }
        // 稳定排序：同 PTS（理论无仅容错）保持解码序。
        got.sort_by_key(|(pts, ..)| *pts);
        let mut frames = Vec::new();
        for (_, w, h, y, uv) in got {
            if let Some((u, v)) = crate::codecs::split_uv_planes(&uv, w, h) {
                self.push_frame(w as u32, h as u32, y, u, v, &mut frames);
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
            let dev = match crate::codecs::hw_vulkan_device() {
                Some(d) => d,
                None => {
                    crate::codecs::note_hw_fallback();
                    self.vk_failed = true;
                    return None;
                }
            };
            match dev.create_bytes_decoder_h264(gpu_video::parameters::DecoderParameters::default())
            {
                Ok(dec) => self.vk_dec = Some(dec),
                Err(_) => {
                    crate::codecs::note_hw_fallback();
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
pub(crate) fn finish_decode(dec: &mut TrackDecoder) -> Result<Vec<Yuv420>, String> {
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
