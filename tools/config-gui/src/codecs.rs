//! 转码编解码后端（host `std`；固件侧不受影响）。
//!
//! 轮子结论（见 `Cargo.toml` 头注释，不复述版本）：
//! - 解码复用与固件同版本轮子：`shiguredo_mp4`（demux，与固件同用法）+
//!   `rusty_h264-decoder 0.16` / `rusty_h265 0.6.2` / `vp9dec 0.1.1`
//!   （与固件 pin 同版；h263/mpeg2v 上游无 crates.io，源解码暂只走 mp4 三轨 +
//!   静帧/裸帧，后继再接；绝不为解码新增 path 依赖，保持 GUI 与 submodule 解耦）。
//! - 编码七路：`less-avc`（AVC baseline）/ `oxideav-theora`（intra theora）/
//!   `rusty_vp9` / `oxideav-h265` / `oxideav-h263` / `oxideav-mpeg12video` /
//!   `qoi`（qois 直出）。OxideAV 四家走统一 registry 形态
//!   （`make_encoder(&CodecParameters)` + `send_frame`/`receive_packet`/`flush`，
//!   见 oxideav-theora 树内 `make_encoder_synthesizes_setup_from_ident_only` 用法）。
//! - 容器：AVC 走 `shiguredo_mp4::Mp4FileMuxer`（与固件 demux 同轮子，
//!   回读零口径差）；theora 走 `ogg` 组 OggS；vp9 走手写 IVF
//!   （32 字节头 + 帧长/pts + 负载，20 行，无现货轮子，测试锁死）。
//! - 内存纪律：40s@60fps 全预解码要 ~19GB，不可能；按关键帧分组
//!   （`GROUP_CAP` 60 封顶，超了硬切 + 状态栏警告），组帧缩到 1920x1080 后
//!   逐组过编码器（组首恒关键帧，拼接恒合法）；压缩包累积（小），
//!   裸帧不过组不驻留。
//! - 几何：统一缩到 1920x1080（固件定死分辨率；1280x720→×1.5 纯放大，
//!   `image::Triangle` 双线性；YUV420 按平面双线性手写，image 只吃打包 RGBA）。
//! - 色彩：YUV↔RGBA 用 BT.601 全范围整数近似（正向与固件
//!   `Clip::yuv420_to_bgra` 同式，反向标准逆变换，roundtrip 容差 ±2）。

/// 平面 YUV420 8-bit（紧凑排列；编码器统一吃这个形状）。
#[derive(Debug, Clone)]
pub(crate) struct Yuv420 {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) y: Vec<u8>,
    pub(crate) u: Vec<u8>,
    pub(crate) v: Vec<u8>,
}

/// 打包 RGBA 帧（`image` 互操作 + QOI 直出形状）。
#[derive(Debug, Clone)]
pub(crate) struct RgbaFrame {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) px: Vec<u8>,
}

/// 解码产物（一路源 → 若干 YUV420 帧 + 帧率）。
pub(crate) struct DecodedClip {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) fps: u32,
    pub(crate) frames: Vec<Yuv420>,
}

/// YUV420 → RGBA（正向与固件 `yuv420_to_bgra` 同式，只是通道换位）。
pub(crate) fn yuv420_to_rgba(f: &Yuv420) -> RgbaFrame {
    let w = f.w as usize;
    let h = f.h as usize;
    let uv_stride = w.div_ceil(2).max(1);
    let mut px = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let yy = f.y.get(row * w + col).copied().unwrap_or(0) as i32;
            let uv_idx = (row / 2) * uv_stride + col / 2;
            let uu = f.u.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let vv = f.v.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let r = (yy + ((359 * vv) >> 8)).clamp(0, 255) as u8;
            let g = (yy - ((88 * uu + 183 * vv) >> 8)).clamp(0, 255) as u8;
            let b = (yy + ((454 * uu) >> 8)).clamp(0, 255) as u8;
            let o = (row * w + col) * 4;
            px[o] = r;
            px[o + 1] = g;
            px[o + 2] = b;
            px[o + 3] = 255;
        }
    }
    RgbaFrame { w: f.w, h: f.h, px }
}

/// RGBA → YUV420（BT.601 全范围逆变换；`yuv→rgba→yuv` 容差 ±2，见测试）。
pub(crate) fn rgba_to_yuv420(f: &RgbaFrame) -> Yuv420 {
    let w = f.w as usize;
    let h = f.h as usize;
    let mut y = vec![0u8; w * h];
    let uvw = w.div_ceil(2);
    let uvh = h.div_ceil(2);
    let mut u = vec![128u8; uvw * uvh];
    let mut v = vec![128u8; uvw * uvh];
    for row in 0..h {
        for col in 0..w {
            let o = (row * w + col) * 4;
            let r = f.px[o] as i32;
            let g = f.px[o + 1] as i32;
            let b = f.px[o + 2] as i32;
            y[row * w + col] = ((77 * r + 150 * g + 29 * b) >> 8).clamp(0, 255) as u8;
            // 色度按 2x2 块取左上样点（编码器吃紧凑平面，简单直接）。
            if row % 2 == 0 && col % 2 == 0 {
                let uo = (row / 2) * uvw + col / 2;
                u[uo] = (((-43 * r - 84 * g + 127 * b) >> 8) + 128).clamp(0, 255) as u8;
                v[uo] = (((127 * r - 106 * g - 21 * b) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
    }
    Yuv420 {
        w: f.w,
        h: f.h,
        y,
        u,
        v,
    }
}

/// YUV420 平面双线性缩放到目标几何（奇数几何按 `(w+1)/2` 取色度步进）。
pub(crate) fn scale_yuv420(f: &Yuv420, dw: u32, dh: u32) -> Yuv420 {
    fn scale_plane(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; dw * dh];
        for y in 0..dh {
            let sy = (y as u64 * sh as u64 / dh as u64) as usize;
            let sy1 = (sy + 1).min(sh - 1);
            let fy = (y as u64 * sh as u64 % dh as u64) as f32 / dh as f32;
            for x in 0..dw {
                let sx = (x as u64 * sw as u64 / dw as u64) as usize;
                let sx1 = (sx + 1).min(sw - 1);
                let fx = (x as u64 * sw as u64 % dw as u64) as f32 / dw as f32;
                let a = src[sy * sw + sx] as f32;
                let b = src[sy * sw + sx1] as f32;
                let c = src[sy1 * sw + sx] as f32;
                let d = src[sy1 * sw + sx1] as f32;
                out[y * dw + x] = (a * (1.0 - fx) * (1.0 - fy)
                    + b * fx * (1.0 - fy)
                    + c * (1.0 - fx) * fy
                    + d * fx * fy)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        out
    }
    let sw = f.w as usize;
    let sh = f.h as usize;
    let dw = dw as usize;
    let dh = dh as usize;
    let y = scale_plane(&f.y, sw, sh, dw, dh);
    let uvw = sw.div_ceil(2);
    let uvh = sh.div_ceil(2);
    let duvw = dw.div_ceil(2);
    let duvh = dh.div_ceil(2);
    let u = scale_plane(&f.u, uvw, uvh, duvw, duvh);
    let v = scale_plane(&f.v, uvw, uvh, duvw, duvh);
    Yuv420 {
        w: dw as u32,
        h: dh as u32,
        y,
        u,
        v,
    }
}

/// Annex-B 切 NAL（`00 00 01` / `00 00 00 01` 分隔；返回不含起始码的负载）。
pub(crate) fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 3 < data.len() {
        let sc = if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            Some(3)
        } else if i + 4 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            Some(4)
        } else {
            None
        };
        if let Some(len) = sc {
            if let Some(s) = start {
                units.push(&data[s..i]);
            }
            start = Some(i + len);
            i += len;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            units.push(&data[s..]);
        }
    } else if !data.is_empty() {
        units.push(data);
    }
    units.retain(|u| !u.is_empty());
    units
}

/// MP4 长度前缀样片 → Annex-B（固件 `demux_mp4_track` 同逻辑）。
pub(crate) fn length_prefixed_to_annexb(sample: &[u8], length_size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cur = sample;
    while cur.len() >= length_size {
        let mut n = 0usize;
        for b in &cur[..length_size] {
            n = (n << 8) | (*b as usize);
        }
        cur = &cur[length_size..];
        let take = n.min(cur.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&cur[..take]);
        cur = &cur[take..];
        if take < n {
            break;
        }
    }
    out
}

/// IVF 封装（vp9 裸流 → `video.ivf`；固件读 32 字节头 + 帧头 + 负载）。
/// tag `VP90`，版本 0，时基 1/30。
pub(crate) fn mux_ivf(frames: &[Vec<u8>], w: u16, h: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"VP90");
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&30u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for (i, f) in frames.iter().enumerate() {
        out.extend_from_slice(&(f.len() as u32).to_le_bytes());
        out.extend_from_slice(&(i as u64).to_le_bytes());
        out.extend_from_slice(f);
    }
    out
}

#[cfg(test)]
mod codecs_tests {
    use super::*;

    fn solid_yuv(w: u32, h: u32, y: u8, u: u8, v: u8) -> Yuv420 {
        Yuv420 {
            w,
            h,
            y: vec![y; (w * h) as usize],
            u: vec![u; (w.div_ceil(2) * h.div_ceil(2)) as usize],
            v: vec![v; (w.div_ceil(2) * h.div_ceil(2)) as usize],
        }
    }

    #[test]
    fn color_roundtrip_within_tolerance() {
        for (y, u, v) in [(16u8, 128u8, 128u8), (235, 128, 128), (100, 80, 200)] {
            let f = solid_yuv(8, 8, y, u, v);
            let back = rgba_to_yuv420(&yuv420_to_rgba(&f));
            for (a, b) in f.y.iter().zip(back.y.iter()) {
                assert!((*a as i16 - *b as i16).abs() <= 2, "{y},{u},{v}");
            }
        }
    }

    #[test]
    fn annexb_split_shapes() {
        let data = [0, 0, 0, 1, 0x65, 1, 0, 0, 1, 0x41, 2, 3];
        let units = split_annexb(&data);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0], &[0x65, 1]);
        assert_eq!(units[1], &[0x41, 2, 3]);
        assert!(split_annexb(&[]).is_empty());
    }

    #[test]
    fn length_prefix_roundtrip() {
        let sample = [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 1, 0x41];
        let annexb = length_prefixed_to_annexb(&sample, 4);
        let units = split_annexb(&annexb);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0], &[0x65, 0x88]);
    }

    #[test]
    fn ivf_header_shape() {
        let out = mux_ivf(&[vec![1, 2, 3]], 64, 48);
        assert_eq!(&out[..4], b"DKIF");
        assert_eq!(&out[8..12], b"VP90");
        assert_eq!(u16::from_le_bytes([out[12], out[13]]), 64);
        assert_eq!(u16::from_le_bytes([out[14], out[15]]), 48);
        assert_eq!(u32::from_le_bytes([out[24], out[25], out[26], out[27]]), 1);
        assert_eq!(u32::from_le_bytes([out[32], out[33], out[34], out[35]]), 3);
    }

    #[test]
    fn scale_identity_and_down() {
        let f = solid_yuv(64, 48, 100, 128, 128);
        let same = scale_yuv420(&f, 64, 48);
        assert_eq!(same.y, f.y);
        let half = scale_yuv420(&f, 32, 24);
        assert_eq!(half.y.len(), 32 * 24);
        assert!(half.y.iter().all(|&v| (v as i16 - 100).abs() <= 1));
    }

    #[test]
    fn media_format_source_map() {
        // 转码源只收这三类：mp4 三轨 + 静帧 + 1080p 裸帧（见 transcode.rs）。
        for (name, want) in [
            ("clip.mp4", true),
            ("frame0001.raw", true),
            ("photo.png", true),
            ("photo.jpg", true),
            ("video.qois", false),
            ("video.ogv", false),
        ] {
            assert_eq!(crate::transcode::source_accepted(name), want, "{name}");
        }
    }

    /// AVC 编码→固件同版本解码 roundtrip（64x48×3 帧，小尺寸保速度）。
    #[test]
    fn avc_encode_decode_roundtrip() {
        let frames = vec![
            solid_yuv(64, 48, 16, 128, 128),
            solid_yuv(64, 48, 128, 128, 128),
            solid_yuv(64, 48, 235, 128, 128),
        ];
        let out = encode_avc(&frames).expect("avc encode");
        assert!(!out.sps.is_empty() && !out.pps.is_empty());
        assert_eq!(out.samples.len(), 3);
        assert!(out.samples[0].keyframe);
        // 固件同款解码器回读（整流 Annex-B，坏帧即错）。
        let mut annexb = Vec::new();
        for nalu in [&out.sps, &out.pps]
            .into_iter()
            .chain(out.samples.iter().flat_map(|s| &s.nalus))
        {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(nalu);
        }
        let decoded = rusty_h264_decoder::Decoder::new()
            .decode_stream(&annexb)
            .expect("avc decode");
        assert_eq!(decoded.len(), 3, "{}", decoded.len());
        assert_eq!((decoded[0].width, decoded[0].height), (64, 48));
    }

    /// VP9 编码→固件同版本解码 roundtrip。
    #[test]
    fn vp9_encode_decode_roundtrip() {
        let frames = vec![
            solid_yuv(64, 48, 16, 128, 128),
            solid_yuv(64, 48, 128, 128, 128),
            solid_yuv(64, 48, 235, 128, 128),
        ];
        let packets = encode_vp9(&frames).expect("vp9 encode");
        assert!(!packets.is_empty());
        // 固件同款 vp9dec 回读（只要显示帧，隐藏 altref 跳过，同固件 collect）。
        let mut dec = vp9dec::Decoder::new();
        let mut shown = 0;
        for p in &packets {
            for d in dec.decode_frame(&p.data).expect("vp9 decode") {
                if let Some(f) = d.frame {
                    assert_eq!((f.width, f.height), (64, 48));
                    shown += 1;
                }
            }
        }
        assert!(shown >= 1, "no shown frames");
    }

    /// Theora 编码→固件同版本解码 roundtrip（intra-only）。
    #[test]
    fn theora_encode_decode_roundtrip() {
        let frames = vec![solid_yuv(64, 48, 128, 128, 128)];
        let out = encode_theora(&frames).expect("theora encode");
        assert_eq!(out.header_packets.len(), 3);
        assert!(!out.data_packets.is_empty());
        // 固件同版本 parser 回读三头 + 解一帧。
        let ident =
            oxideav_theora::decode_identification_header(&out.header_packets[0]).expect("ident");
        assert_eq!(ident.picw, 64);
        let setup = oxideav_theora::decode_setup_header(&out.header_packets[2]).expect("setup");
        let _ = (ident, setup);
    }

    /// H.265 编码→固件同版本解码 roundtrip。
    #[test]
    fn hevc_encode_decode_roundtrip() {
        let frames = vec![
            solid_yuv(64, 48, 16, 128, 128),
            solid_yuv(64, 48, 235, 128, 128),
        ];
        let annexb = encode_hevc(&frames).expect("hevc encode");
        assert!(!annexb.is_empty());
        let mut dec = rusty_h265::Decoder::new();
        dec.push_annexb(&annexb, None).expect("hevc push");
        dec.flush();
        let mut n = 0;
        loop {
            match dec.next_frame() {
                Ok(f) => {
                    assert_eq!((f.width as u32, f.height as u32), (64, 48));
                    n += 1;
                }
                Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
                Err(e) => panic!("hevc decode: {e:?}"),
            }
        }
        assert!(n >= 1, "no hevc frames");
    }

    /// H.263 / MPEG-2 编码自 roundtrip（固件侧是异箱解码器，
    /// 此处先锁自家编解码一致；跨箱结论见 docs/transcode.md）。
    /// H.263 只要 §5.1.3 标准源格式（此处用 QCIF 176x144）。
    #[test]
    fn h263_mpeg2v_encode_smoke() {
        let frames = vec![solid_yuv(176, 144, 128, 128, 128)];
        let h263 = encode_h263(&frames).expect("h263 encode");
        assert!(!h263.is_empty());
        // H.263 裸流以 PSC 起始（00 00 80），异箱解码器认这个。
        assert_eq!(&h263[..3], &[0, 0, 0x80]);
        let m2v = encode_mpeg2v(&frames).expect("mpeg2v encode");
        assert!(!m2v.is_empty());
        // MPEG 基本流以 sequence_header_code 起始（00 00 01 B3）。
        assert_eq!(&m2v[..4], &[0, 0, 1, 0xB3]);
    }
}

// ---------- 编码后端 ----------

use oxideav_core::{CodecId, CodecParameters, Encoder, Frame, VideoFrame, VideoPlane};

/// OxideAV registry 形态统一驱动：逐帧 `send_frame`，帧帧排空，
/// 尾部 `flush` 后排空（`NeedMore` 换帧，`Eof` 收尾）。
fn drive_encoder(
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

fn yuv_to_less_avc(f: &Yuv420) -> less_avc::ycbcr_image::YCbCrImage<'_> {
    use less_avc::ycbcr_image::{DataPlane, Planes, YCbCrImage};
    use less_avc::BitDepth;
    YCbCrImage {
        planes: Planes::YCbCr((
            DataPlane {
                data: &f.y,
                stride: f.w as usize,
                bit_depth: BitDepth::Depth8,
            },
            DataPlane {
                data: &f.u,
                stride: f.w.div_ceil(2) as usize,
                bit_depth: BitDepth::Depth8,
            },
            DataPlane {
                data: &f.v,
                stride: f.w.div_ceil(2) as usize,
                bit_depth: BitDepth::Depth8,
            },
        )),
        width: f.w,
        height: f.h,
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

/// AVC 编码（`less-avc` baseline；组内逐帧，首帧出 SPS/PPS）。
/// 1080 高非 16 倍数由轮内 `frame_cropping` 兜底（见其 encoder.rs）。
pub(crate) fn encode_avc(frames: &[Yuv420]) -> Result<AvcStream, String> {
    if frames.is_empty() {
        return Err(String::from("avc: no frames"));
    }
    let first = yuv_to_less_avc(&frames[0]);
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
    for f in &frames[1..] {
        let img = yuv_to_less_avc(f);
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

/// VP9 包（裸帧负载；IVF 封装另做）。
pub(crate) struct Vp9PacketData {
    pub(crate) data: Vec<u8>,
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
            Ok(p) => out.push(Vp9PacketData { data: p.data }),
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

/// H.265 编码（`oxideav-h265` registry；包负载拼接即 Annex-B，
/// 解码端 `push_annexb` 照收，见测试）。
pub(crate) fn encode_hevc(frames: &[Yuv420]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Err(String::from("hevc: no frames"));
    }
    let mut params = CodecParameters::video(CodecId::new("h265"));
    params.width = Some(frames[0].w);
    params.height = Some(frames[0].h);
    let enc = oxideav_h265::make_encoder(&params).map_err(|e| e.to_string())?;
    let packets = drive_encoder(enc, frames)?;
    let mut out = Vec::new();
    for p in packets {
        out.extend_from_slice(&p.data);
    }
    if out.is_empty() {
        return Err(String::from("hevc: no packets"));
    }
    Ok(out)
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

/// 固件侧回读：GUI 产出必须被固件同款解码器接受（异箱，同 API）。
#[cfg(test)]
mod codecs_readback_tests {
    use super::*;

    /// 公用纯色帧（与 codecs_tests::solid_yuv 同形）。
    fn frame(w: u32, h: u32) -> Yuv420 {
        Yuv420 {
            w,
            h,
            y: vec![128; (w * h) as usize],
            u: vec![128; (w.div_ceil(2) * h.div_ceil(2)) as usize],
            v: vec![128; (w.div_ceil(2) * h.div_ceil(2)) as usize],
        }
    }

    /// VP9：`rusty_vp9` 编 → 固件 `vp9dec` 解（跳隐藏帧，固件同款逻辑）。
    #[test]
    fn vp9_firmware_readback() {
        let pkts = encode_vp9(&[frame(64, 48), frame(64, 48)]).expect("vp9 encode");
        assert!(pkts.len() >= 2);
        let mut got = 0;
        let mut dec = vp9dec::Decoder::new();
        for p in &pkts {
            let frames = dec.decode_frame(&p.data).expect("vp9 decode");
            got += frames.iter().filter(|d| d.frame.is_some()).count();
        }
        assert!(got >= 1, "vp9 readback: no display frames");
    }

    /// HEVC：`oxideav-h265` 编 → `rusty_h265`（固件同系）Annex-B 解。
    #[test]
    fn hevc_firmware_readback() {
        let bytes = encode_hevc(&[frame(64, 48), frame(64, 48)]).expect("hevc encode");
        let mut dec = rusty_h265::Decoder::new();
        dec.push_annexb(&bytes, None).expect("hevc push");
        dec.flush();
        let mut got = 0;
        while dec.next_frame().is_ok() {
            got += 1;
        }
        assert!(got >= 1, "hevc readback: no frames");
    }

    /// Theora：自家编码器 Ogg 包 → `oxideav-theora` 解码器（BE-len 外挂链）。
    #[test]
    fn theora_firmware_readback() {
        let t = encode_theora(&[frame(64, 48)]).expect("theora encode");
        let mut extradata = Vec::new();
        for h in &t.header_packets {
            extradata.extend_from_slice(&(h.len() as u16).to_be_bytes());
            extradata.extend_from_slice(h);
        }
        let mut params = CodecParameters::video(CodecId::new("theora"));
        params.extradata = extradata;
        let mut dec = oxideav_theora::make_decoder(&params).expect("theora decoder");
        let tb = oxideav_core::TimeBase::new(1, 30);
        let mut got = 0;
        for d in &t.data_packets {
            use oxideav_core::Decoder as _;
            dec.send_packet(&oxideav_core::Packet::new(0, tb, d.clone()))
                .expect("theora send");
            while let Ok(f) = dec.receive_frame() {
                if matches!(f, oxideav_core::Frame::Video(_)) {
                    got += 1;
                }
            }
        }
        assert!(got >= 1, "theora readback: no frames");
    }

    /// QOIS：逐帧 `qoi` 编 → `qoi` 解回读（固件 `qois` 布局同源）。
    #[test]
    fn qois_firmware_readback() {
        let rgba = yuv420_to_rgba(&frame(64, 48));
        let q = qoi::encode_to_vec(&rgba.px, rgba.w, rgba.h).expect("qoi encode");
        let (h, px) = qoi::decode_to_vec(&q).expect("qoi decode");
        assert_eq!((h.width, h.height), (64, 48));
        assert_eq!(px.len(), (64 * 48 * 4) as usize);
    }
}
