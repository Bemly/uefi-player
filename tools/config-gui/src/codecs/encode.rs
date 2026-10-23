//! CPU 编码后端（OxideAV registry 驱动 + avc/hevc/vp9/theora/h263/mpeg2v；拆自 `codecs.rs`，行为零改）。

use super::*;

// ---------- 编码后端 ----------

use oxideav_core::{CodecId, CodecParameters, Encoder, Frame, VideoFrame, VideoPlane};

/// OxideAV registry 形态统一驱动：逐帧 `send_frame`，帧帧排空，
/// 尾部 `flush` 后排空（`NeedMore` 换帧，`Eof` 收尾）。
pub(crate) fn drive_encoder(
    mut enc: Box<dyn Encoder>,
    frames: &[Yuv420],
) -> Result<Vec<oxideav_core::Packet>, String> {
    let mut out = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let vf = VideoFrame {
            pts: Some(i as i64),
            planes: vec![
                VideoPlane {
                    stride: f.w as usize,
                    data: f.y.clone(),
                },
                VideoPlane {
                    stride: f.w.div_ceil(2) as usize,
                    data: f.u.clone(),
                },
                VideoPlane {
                    stride: f.w.div_ceil(2) as usize,
                    data: f.v.clone(),
                },
            ],
        };
        enc.send_frame(&Frame::Video(vf))
            .map_err(|e| e.to_string())?;
        loop {
            match enc.receive_packet() {
                Ok(p) => out.push(p),
                Err(oxideav_core::Error::NeedMore) => break,
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    enc.flush().map_err(|e| e.to_string())?;
    loop {
        match enc.receive_packet() {
            Ok(p) => out.push(p),
            // 有的实现排空后报 Eof，有的报 NeedMore（theora intra-only 无缓冲）——都算收尾。
            Err(oxideav_core::Error::Eof) | Err(oxideav_core::Error::NeedMore) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}

/// 平面补齐到 stride/行数满足 less-avc `check_sizes`（边缘复制；
/// pic 尺寸不变，轮内 `frame_cropping` 按原尺寸写 SPS 裁剪）。
pub(crate) fn pad_plane_16(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize) {
    let dw = w.next_multiple_of(16).max(16);
    let dh = h.next_multiple_of(16).max(16);
    if dw == w && dh == h {
        return (src.to_vec(), w);
    }
    let mut out = vec![0u8; dw * dh];
    for y in 0..dh {
        let sy = y.min(h.saturating_sub(1));
        for x in 0..dw {
            let sx = x.min(w.saturating_sub(1));
            out[y * dw + x] = *src.get(sy * w + sx).unwrap_or(&0);
        }
    }
    (out, dw)
}

/// AVC 输入帧（owned 缓冲 + 原 pic 几何；less-avc 要求缓冲按 16 对齐，
/// `width`/`height` 报原尺寸，轮内 `frame_cropping` 写 SPS 裁剪，
/// 合规解码器恢复原几何，见 `pad_plane_16` 与 1920x1080 回读测试）。
pub(crate) struct AvcInputFrame {
    y: Vec<u8>,
    ystride: usize,
    u: Vec<u8>,
    ustride: usize,
    v: Vec<u8>,
    vstride: usize,
    w: u32,
    h: u32,
}

impl AvcInputFrame {
    fn from_yuv420(f: &Yuv420) -> Self {
        let (y, ystride) = pad_plane_16(&f.y, f.w as usize, f.h as usize);
        let uvw = f.w.div_ceil(2) as usize;
        let uvh = f.h.div_ceil(2) as usize;
        let (u, ustride) = pad_plane_16(&f.u, uvw, uvh);
        let (v, vstride) = pad_plane_16(&f.v, uvw, uvh);
        Self {
            y,
            ystride,
            u,
            ustride,
            v,
            vstride,
            w: f.w,
            h: f.h,
        }
    }

    fn view(&self) -> less_avc::ycbcr_image::YCbCrImage<'_> {
        use less_avc::ycbcr_image::{DataPlane, Planes, YCbCrImage};
        use less_avc::BitDepth;
        YCbCrImage {
            planes: Planes::YCbCr((
                DataPlane {
                    data: &self.y,
                    stride: self.ystride,
                    bit_depth: BitDepth::Depth8,
                },
                DataPlane {
                    data: &self.u,
                    stride: self.ustride,
                    bit_depth: BitDepth::Depth8,
                },
                DataPlane {
                    data: &self.v,
                    stride: self.vstride,
                    bit_depth: BitDepth::Depth8,
                },
            )),
            width: self.w,
            height: self.h,
        }
    }
}
/// AVC 样片（MP4 封装用：NAL 裸负载 + 关键帧标记）。
pub(crate) struct AvcSample {
    pub(crate) keyframe: bool,
    pub(crate) nalus: Vec<Vec<u8>>,
}

/// AVC 整流（SPS/PPS + 样片；`to_mp4` 另做长度前缀包装）。
pub(crate) struct AvcStream {
    pub(crate) sps: Vec<u8>,
    pub(crate) pps: Vec<u8>,
    pub(crate) samples: Vec<AvcSample>,
}

/// AVC 编码（硬解优先，失败回退 CPU；段首恒关键帧，30 帧一段语义不变）。
/// 硬解输出与 CPU 同形状（SPS/PPS + 样片），下游 `to_mp4` 零改。
pub(crate) fn encode_avc(frames: &[Yuv420]) -> Result<AvcStream, String> {
    match encode_avc_hw(frames) {
        Ok(s) => Ok(s),
        Err(_) => {
            note_hw_fallback();
            encode_avc_cpu(frames)
        }
    }
}

/// AVC 编码 CPU 兜底（`less-avc` baseline；组内逐帧，首帧出 SPS/PPS）。
/// 非 16 对齐几何（如 1920x1080）先补齐缓冲、pic 报原尺寸，
/// 轮内 `frame_cropping` 写 SPS 裁剪（见 `AvcInputFrame`）。
pub(crate) fn encode_avc_cpu(frames: &[Yuv420]) -> Result<AvcStream, String> {
    if frames.is_empty() {
        return Err(String::from("avc: no frames"));
    }
    let padded: Vec<AvcInputFrame> = frames.iter().map(AvcInputFrame::from_yuv420).collect();
    let first = padded[0].view();
    let (init, mut enc) =
        less_avc::LessEncoder::new(&first).map_err(|e| format!("avc init: {e:?}"))?;
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    for u in split_annexb(&init.sps.to_annex_b_data()) {
        if !u.is_empty() && u[0] & 0x1F == 7 {
            sps = u.to_vec();
        }
    }
    for u in split_annexb(&init.pps.to_annex_b_data()) {
        if !u.is_empty() && u[0] & 0x1F == 8 {
            pps = u.to_vec();
        }
    }
    if sps.is_empty() || pps.is_empty() {
        return Err(String::from("avc: missing sps/pps"));
    }
    let mut samples = Vec::new();
    // `new` 已顺手编完首帧（`init.frame`），直接收下，剩下的逐帧走。
    for bytes in [init.frame.to_annex_b_data()] {
        let mut nalus = Vec::new();
        let mut keyframe = false;
        for u in split_annexb(&bytes) {
            if u.is_empty() {
                continue;
            }
            if u[0] & 0x1F == 5 {
                keyframe = true;
            }
            nalus.push(u.to_vec());
        }
        samples.push(AvcSample { keyframe, nalus });
    }
    for f in &padded[1..] {
        let img = f.view();
        let nalu = enc.encode(&img).map_err(|e| format!("avc encode: {e:?}"))?;
        let bytes = nalu.to_annex_b_data();
        let mut nalus = Vec::new();
        let mut keyframe = false;
        for u in split_annexb(&bytes) {
            if u.is_empty() {
                continue;
            }
            if u[0] & 0x1F == 5 {
                keyframe = true;
            }
            nalus.push(u.to_vec());
        }
        samples.push(AvcSample { keyframe, nalus });
    }
    if !samples.is_empty() {
        samples[0].keyframe = true;
    }
    Ok(AvcStream { sps, pps, samples })
}

/// VP9 包（裸帧负载 + 关键帧标记；MP4 stss 用）。
pub(crate) struct Vp9PacketData {
    pub(crate) data: Vec<u8>,
    pub(crate) keyframe: bool,
}

/// VP9 编码（`rusty_vp9`，恒质量 crf32 + lookahead 8 + speed 3；
/// 速度/体积折中见其文档 honesty 注）。
pub(crate) fn encode_vp9(frames: &[Yuv420]) -> Result<Vec<Vp9PacketData>, String> {
    if frames.is_empty() {
        return Err(String::from("vp9: no frames"));
    }
    let mut enc = rusty_vp9::Vp9Encoder::default();
    enc.configure(&rusty_vp9::Vp9EncoderConfig {
        crf: Some(32),
        lag: Some(8),
        speed: Some(3),
        ..Default::default()
    })
    .map_err(|e| format!("vp9 configure: {e:?}"))?;
    for f in frames {
        let w = f.w as usize;
        let uvw = f.w.div_ceil(2) as usize;
        enc.push_frame([&f.y[..], &f.u[..], &f.v[..]], [w, uvw, uvw], f.w, f.h)
            .map_err(|e| format!("vp9 push: {e:?}"))?;
    }
    enc.flush();
    let mut out = Vec::new();
    loop {
        match enc.next_packet() {
            Ok(p) => out.push(Vp9PacketData {
                data: p.data,
                keyframe: p.keyframe,
            }),
            Err(rusty_vp9::Error::Eof) => break,
            Err(e) => return Err(format!("vp9 packet: {e:?}")),
        }
    }
    if out.is_empty() {
        return Err(String::from("vp9: no packets"));
    }
    Ok(out)
}

/// Theora 整流（三头 + 数据包；Ogg 封装另做）。
pub(crate) struct TheoraStream {
    pub(crate) header_packets: Vec<Vec<u8>>,
    pub(crate) data_packets: Vec<Vec<u8>>,
}

/// Theora 编码（`oxideav-theora` registry intra-only，`qi` 32；
/// setup 用轮内 VP3 默认合成，extradata 只带 ident，见其 `make_encoder`）。
pub(crate) fn encode_theora(frames: &[Yuv420]) -> Result<TheoraStream, String> {
    if frames.is_empty() {
        return Err(String::from("theora: no frames"));
    }
    let w = frames[0].w;
    let h = frames[0].h;
    let ident = oxideav_theora::TheoraIdentHeader::for_picture(
        w,
        h,
        oxideav_theora::PixelFormat::Yuv420,
        30,
        1,
    )
    .map_err(|e| format!("theora ident: {e}"))?;
    let ident_pkt =
        oxideav_theora::encode_identification_header(&ident).map_err(|e| format!("{e}"))?;
    let mut ed = Vec::new();
    ed.extend_from_slice(&(ident_pkt.len() as u16).to_be_bytes());
    ed.extend_from_slice(&ident_pkt);
    let mut params = CodecParameters::video(CodecId::new(oxideav_theora::THEORA_CODEC_ID));
    params.extradata = ed;
    params
        .options
        .insert(String::from("qi"), String::from("32"));
    let enc = oxideav_theora::make_encoder(&params).map_err(|e| e.to_string())?;
    let packets = drive_encoder(enc, frames)?;
    let mut header_packets = Vec::new();
    let mut data_packets = Vec::new();
    for p in packets {
        if p.flags.header {
            header_packets.push(p.data);
        } else {
            data_packets.push(p.data);
        }
    }
    if header_packets.len() != 3 || data_packets.is_empty() {
        return Err(format!(
            "theora: headers={} data={}",
            header_packets.len(),
            data_packets.len()
        ));
    }
    Ok(TheoraStream {
        header_packets,
        data_packets,
    })
}

/// H.265 样片（长度前缀包装前形态；MP4 stss 用 keyframe）。
pub(crate) struct HevcSample {
    pub(crate) keyframe: bool,
    pub(crate) nalus: Vec<Vec<u8>>,
}

/// H.265 整流（VPS/SPS/PPS + 样片；MP4 hvc1 包装另做）。
pub(crate) struct HevcStream {
    pub(crate) vps: Vec<u8>,
    pub(crate) sps: Vec<u8>,
    pub(crate) pps: Vec<u8>,
    pub(crate) samples: Vec<HevcSample>,
}

/// H.265 编码（硬解优先，失败回退 CPU；包负载按 Annex-B 拼，
/// NAL 类型 32/33/34 收进参数集，19/20/21 判关键帧）。
pub(crate) fn encode_hevc(frames: &[Yuv420]) -> Result<HevcStream, String> {
    match encode_hevc_hw(frames) {
        Ok(s) => Ok(s),
        Err(_) => {
            note_hw_fallback();
            encode_hevc_cpu(frames)
        }
    }
}

/// H.265 编码 CPU 兜底（`oxideav-h265` registry；包负载按 Annex-B 拼，
/// NAL 类型 32/33/34 收进参数集，19/20/21 判关键帧）。
pub(crate) fn encode_hevc_cpu(frames: &[Yuv420]) -> Result<HevcStream, String> {
    if frames.is_empty() {
        return Err(String::from("hevc: no frames"));
    }
    let mut params = CodecParameters::video(CodecId::new("h265"));
    params.width = Some(frames[0].w);
    params.height = Some(frames[0].h);
    let enc = oxideav_h265::make_encoder(&params).map_err(|e| e.to_string())?;
    let packets = drive_encoder(enc, frames)?;
    let (mut vps, mut sps, mut pps) = (Vec::new(), Vec::new(), Vec::new());
    let mut samples = Vec::new();
    for p in packets {
        let mut nalus = Vec::new();
        let mut keyframe = false;
        for u in split_annexb(&p.data) {
            if u.is_empty() {
                continue;
            }
            match (u[0] >> 1) & 0x3F {
                32 if vps.is_empty() => vps = u.to_vec(),
                33 if sps.is_empty() => sps = u.to_vec(),
                34 if pps.is_empty() => pps = u.to_vec(),
                19..=21 => {
                    keyframe = true;
                    nalus.push(u.to_vec());
                }
                _ => nalus.push(u.to_vec()),
            }
        }
        // 纯参数集包（若有）不计样片。
        if !nalus.is_empty() {
            samples.push(HevcSample { keyframe, nalus });
        }
    }
    if vps.is_empty() || sps.is_empty() || pps.is_empty() || samples.is_empty() {
        return Err(String::from("hevc: missing param sets or samples"));
    }
    samples[0].keyframe = true;
    Ok(HevcStream {
        vps,
        sps,
        pps,
        samples,
    })
}

/// H.263 编码（`oxideav-h263` registry；裸流 PSC 分隔，固件认这个）。
pub(crate) fn encode_h263(frames: &[Yuv420]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Err(String::from("h263: no frames"));
    }
    let mut params = CodecParameters::video(CodecId::new("h263"));
    params.width = Some(frames[0].w);
    params.height = Some(frames[0].h);
    let enc = oxideav_h263::make_encoder(&params).map_err(|e| e.to_string())?;
    let packets = drive_encoder(enc, frames)?;
    let mut out = Vec::new();
    for p in packets {
        out.extend_from_slice(&p.data);
    }
    if out.is_empty() {
        return Err(String::from("h263: no packets"));
    }
    Ok(out)
}

/// MPEG-1/2 编码（`oxideav-mpeg12video` registry；裸基本流，
/// sequence_header 起始，固件认这个）。
pub(crate) fn encode_mpeg2v(frames: &[Yuv420]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Err(String::from("mpeg2v: no frames"));
    }
    let mut params = CodecParameters::video(CodecId::new("mpeg2video"));
    params.width = Some(frames[0].w);
    params.height = Some(frames[0].h);
    let enc = oxideav_mpeg12video::encoder::make_encoder(&params).map_err(|e| e.to_string())?;
    let packets = drive_encoder(enc, frames)?;
    let mut out = Vec::new();
    for p in packets {
        out.extend_from_slice(&p.data);
    }
    if out.is_empty() {
        return Err(String::from("mpeg2v: no packets"));
    }
    Ok(out)
}
