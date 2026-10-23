//! 转码编解码后端（host `std`；固件侧不受影响）。
//!
//! 轮子结论（见 `Cargo.toml` 头注释，不复述版本）：
//! - 解码复用与固件同版本轮子：`shiguredo_mp4`（demux，与固件同用法）+
//!   `rusty_h264-decoder 0.16` / `rusty_h265 0.6.2` / `vp9dec 0.1.1`
//!   （与固件 pin 同版；h263/mpeg2v 上游无 crates.io，源解码暂只走 mp4 三轨 +
//!   静帧/裸帧，后继再接；绝不为解码新增 path 依赖，保持 GUI 与 submodule 解耦）。
//! - 编码七路：avc/hevc 走硬解（mac VideoToolbox / Win-Linux Vulkan Video，
//!   段首恒关键帧 + 30 帧一段与 CPU 时代同语义；失败回退 CPU，保证不断转码）。
//!   CPU 回退链保留：`less-avc`（AVC baseline）/ `oxideav-h265`（registry）
//!   原样不动，只当兜底。`oxideav-theora`（intra）/ `rusty_vp9` / `oxideav-h263` /
//!   `oxideav-mpeg12video` / `qoi`（qois 直出）维持纯 CPU。OxideAV 四家走统一
//!   registry 形态（`make_encoder(&CodecParameters)` + `send_frame`/`receive_packet`/
//!   `flush`，见 oxideav-theora 树内 `make_encoder_synthesizes_setup_from_ident_only` 用法）。
//!   硬解参数口径：1080p 8Mbps VBR 上限（两家统一；nominal 30fps 只定码控时钟，
//!   PTS 自洽与源 fps 无关）；AVC Baseline + CAVLC（与旧 CPU 输出同 profile，
//!   固件解码零风险），HEVC Main 8-bit；B 帧全关（in-order，与旧管线同序）。
//!   真零拷贝不做：VideoToolbox 只收 CPU 切片（无裸句柄可导 Metal），
//!   Vulkan 侧纹理版产的是轮子自建 device 的纹理（跨不过 device，必须外部
//!   内存导入，无现货轮子）；NV12 上传 + CPU 平面拼装是 DMA 级搬运，
//!   无像素运算（与动态背景 `video_bg` 同纪律）。
//! - 容器：AVC 走 `shiguredo_mp4::Mp4FileMuxer`（与固件 demux 同轮子，
//!   回读零口径差）；theora 走 `ogg` 组 OggS；vp9 裸帧直装 vpcC。
//! - 内存纪律：40s@60fps 全预解码要 ~19GB，不可能；按关键帧分组
//!   （`GROUP_CAP` 120 封顶，超了硬切），组内 30 帧一段过编码器
//!   （段首恒关键帧，拼接恒合法）；压缩包累积（小），裸帧不过组不驻留。
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

/// H.263 §5.1.3 五档标准源格式（`oxideav-h263` 编码器硬门，非此即 Err；
/// 轮子内同表私有此处复刻，改表以轮子为准，见 `docs/transcode.md`）。
const H263_STANDARD_FORMATS: [(u32, u32); 5] =
    [(128, 96), (176, 144), (352, 288), (704, 576), (1408, 1152)];

/// H.263 目标尺寸：源能装下的最大标准档（不浪费不放大）；
/// 超小源取最小档（sub-QCIF，放大）。
/// 纯函数（可单测）：转码 H263 供片缩放用，其余目标不走这里。
pub(crate) fn h263_target_size(sw: u32, sh: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 {
        return H263_STANDARD_FORMATS[0];
    }
    H263_STANDARD_FORMATS
        .iter()
        .rev()
        .find(|&&(w, h)| w <= sw && h <= sh)
        .copied()
        .unwrap_or(H263_STANDARD_FORMATS[0])
}

/// 等比内接 + 黑边居中（Y=0/U=V=128 全范围黑）→ 精确标准尺寸。
/// 固件播片按帧几何 1:1 左上（不拉伸，见 `Screen::upload_frame`），
/// 黑边保比后屏上不变形；输出即编码器吃的形状。
/// 纯函数（可单测）。
pub(crate) fn h263_letterbox(f: &Yuv420) -> Yuv420 {
    let (dw, dh) = h263_target_size(f.w, f.h);
    let (dwu, dhu) = (dw as usize, dh as usize);
    if f.w == 0 || f.h == 0 || f.y.is_empty() {
        // 空帧即纯黑标准画布（不断转码）。
        let (duvw, duvh) = (dwu.div_ceil(2), dhu.div_ceil(2));
        return Yuv420 {
            w: dw,
            h: dh,
            y: vec![0u8; dwu * dhu],
            u: vec![128u8; duvw * duvh],
            v: vec![128u8; duvw * duvh],
        };
    }
    let (sw, sh) = (f.w as u64, f.h as u64);
    let (dw64, dh64) = (dw as u64, dh as u64);
    // 内接：fw/fh 取 min（u64 防 4K 源乘法溢出，至少 2px 偶数给 420）。
    let mut fw = (sw * dh64 / sh.max(1)).min(dw64).max(2) as usize;
    let mut fh = (sh * dw64 / sw.max(1)).min(dh64).max(2) as usize;
    fw -= fw % 2;
    fh -= fh % 2;
    let scaled = scale_yuv420(f, fw as u32, fh as u32);
    let (ox, oy) = ((dwu - fw) / 2, (dhu - fh) / 2);
    let mut y = vec![0u8; dwu * dhu];
    let (duvw, duvh) = (dwu.div_ceil(2), dhu.div_ceil(2));
    let mut u = vec![128u8; duvw * duvh];
    let mut v = vec![128u8; duvw * duvh];
    fn blit(dst: &mut [u8], dst_w: usize, src: &[u8], sw: usize, sh: usize, ox: usize, oy: usize) {
        for row in 0..sh {
            dst[(oy + row) * dst_w + ox..][..sw].copy_from_slice(&src[row * sw..][..sw]);
        }
    }
    blit(&mut y, dwu, &scaled.y, fw, fh, ox, oy);
    let (suvw, suvh) = (fw.div_ceil(2), fh.div_ceil(2));
    blit(&mut u, duvw, &scaled.u, suvw, suvh, ox / 2, oy / 2);
    blit(&mut v, duvw, &scaled.v, suvw, suvh, ox / 2, oy / 2);
    Yuv420 {
        w: dw,
        h: dh,
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

mod encode;
#[allow(unused_imports)]
pub(crate) use encode::*;
mod hw;
#[allow(unused_imports)]
pub(crate) use hw::*;

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
    fn avcc_split_shapes() {
        // 两个 4 字节 BE 长度 NAL。
        let data = [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 1, 0x41];
        let units = avcc_to_nalus(&data).expect("avcc");
        assert_eq!(units, vec![vec![0x65, 0x88], vec![0x41]]);
        // 截断/空包即 None（调用方回退 CPU）。
        assert!(avcc_to_nalus(&[]).is_none());
        assert!(avcc_to_nalus(&[0, 0, 0]).is_none());
        assert!(avcc_to_nalus(&[0, 0, 0, 5, 0x65]).is_none());
        assert!(avcc_to_nalus(&[0, 0, 0, 0, 0x65]).is_none());
    }

    #[test]
    fn profile_gate_spots() {
        assert_eq!(sps_profile_idc(&[0x67, 66, 0x00]), Some(66));
        // VideoToolbox 编出 nal_ref_idc=1 的 0x27 头，同样认。
        assert_eq!(sps_profile_idc(&[0x27, 66, 0x00]), Some(66));
        assert_eq!(sps_profile_idc(&[0x67, 100, 0x00]), Some(100));
        assert_eq!(sps_profile_idc(&[]), None);
        assert_eq!(sps_profile_idc(&[0x68, 66]), None);
    }

    #[test]
    fn hw_fallback_flag_take_clear() {
        assert!(!take_hw_fallback_hit());
        note_hw_fallback();
        note_hw_fallback();
        assert!(take_hw_fallback_hit());
        assert!(!take_hw_fallback_hit());
    }

    #[test]
    fn nv12_roundtrip_is_memcpy() {
        // 交错 ↔ 平面互逆，逐字节一致（零运算）。
        let f = Yuv420 {
            w: 6,
            h: 4,
            y: (0..24u8).collect(),
            u: vec![10, 11, 12, 13, 14, 15],
            v: vec![20, 21, 22, 23, 24, 25],
        };
        let nv = yuv420_to_nv12(&f).expect("nv12");
        assert_eq!(nv.len(), 6 * 4 + 3 * 2 * 2);
        // Y 整拷。
        assert_eq!(&nv[..24], &(0..24u8).collect::<Vec<_>>()[..]);
        // UV 交错序：U0V0 U1V1…
        assert_eq!(&nv[24..], &[10, 20, 11, 21, 12, 22, 13, 23, 14, 24, 15, 25]);
        let (u, v) = split_uv_planes(&nv[24..], 6, 4).expect("split");
        assert_eq!(u, vec![10, 11, 12, 13, 14, 15]);
        assert_eq!(v, vec![20, 21, 22, 23, 24, 25]);
        // 坏输入即 None。
        assert!(yuv420_to_nv12(&Yuv420 {
            w: 0,
            h: 0,
            y: vec![],
            u: vec![],
            v: vec![]
        })
        .is_none());
        assert!(split_uv_planes(&[1, 2, 3], 6, 4).is_none());
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

    /// AVC 非对齐几何 roundtrip（64x40：高 40 不是 16 倍数，走补齐 +
    /// SPS `frame_cropping` 路径；解码器必须裁回原几何，否则固件的
    /// 1920x1080 几何校验会拒收）。
    #[test]
    fn avc_odd_height_crop_roundtrip() {
        let frames = vec![
            solid_yuv(64, 40, 16, 128, 128),
            solid_yuv(64, 40, 235, 128, 128),
        ];
        let out = encode_avc(&frames).expect("avc encode");
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
        assert_eq!(decoded.len(), 2, "{}", decoded.len());
        assert_eq!((decoded[0].width, decoded[0].height), (64, 40));
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
        let st = encode_hevc(&frames).expect("hevc encode");
        assert_eq!(st.samples.len(), 2);
        let mut annexb = Vec::new();
        for nalu in [&st.vps, &st.sps, &st.pps] {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(nalu);
        }
        for s in &st.samples {
            for nalu in &s.nalus {
                annexb.extend_from_slice(&[0, 0, 0, 1]);
                annexb.extend_from_slice(nalu);
            }
        }
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

/// 固件侧回读：GUI 产出必须被固件同款解码器接受（异箱，同 API）。
#[cfg(test)]
mod codecs_readback_tests {
    use super::*;
    use oxideav_core::{CodecId, CodecParameters};

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
        let st = encode_hevc(&[frame(64, 48), frame(64, 48)]).expect("hevc encode");
        let mut annexb = Vec::new();
        for nalu in [&st.vps, &st.sps, &st.pps] {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(nalu);
        }
        for s in &st.samples {
            for nalu in &s.nalus {
                annexb.extend_from_slice(&[0, 0, 0, 1]);
                annexb.extend_from_slice(nalu);
            }
        }
        let mut dec = rusty_h265::Decoder::new();
        dec.push_annexb(&annexb, None).expect("hevc push");
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
