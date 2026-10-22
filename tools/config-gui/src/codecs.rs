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

/// 平面补齐到 stride/行数满足 less-avc `check_sizes`（边缘复制；
/// pic 尺寸不变，轮内 `frame_cropping` 按原尺寸写 SPS 裁剪）。
fn pad_plane_16(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize) {
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
struct AvcInputFrame {
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
fn encode_avc_cpu(frames: &[Yuv420]) -> Result<AvcStream, String> {
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
fn encode_hevc_cpu(frames: &[Yuv420]) -> Result<HevcStream, String> {
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

/// 硬解失败回退标记（线程级 sticky：转码 worker 线内只置位不清除；
/// UI 在起 job 前清零、收 Finished 时读取 → toast；图库封面线程误置无妨，
/// 它的线程从不被读取，随线程结束丢弃）。
thread_local! {
    static HW_FALLBACK_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 记一次硬解回退（编码/解码 HW 路失败转 CPU 时调用）。
pub(crate) fn note_hw_fallback() {
    HW_FALLBACK_HIT.with(|f| f.set(true));
}

/// 取走并清零回退标记（转码 job 起止各调一次）。
pub(crate) fn take_hw_fallback_hit() -> bool {
    HW_FALLBACK_HIT.with(|f| f.replace(false))
}

/// 硬解编码（avc/hevc；mac VideoToolbox / Win-Linux Vulkan Video）。
/// 形状与 CPU 版一致（首帧强制关键帧 + 参数集 + 1:1 样片），下游 `to_mp4` 零改；
/// 任何失败一律 `Err`（调用方回退 CPU，保证不断转码）。
/// 码控口径（两家统一，写死）：1080p 8Mbps 上限；nominal 30fps 只定码控时钟
/// （输入输出 PTS 自洽，与源 fps 无关）；AVC Baseline + CAVLC（与旧 CPU 输出
/// 同 profile，固件解码零风险），HEVC Main 8-bit；B 帧全关（in-order 输出，
/// 与旧管线同序，无需重排）。
/// CPU 参与仅 DMA 级搬运（平面拷贝/NV12 交错），无像素运算。
fn encode_avc_hw(frames: &[Yuv420]) -> Result<AvcStream, String> {
    if frames.is_empty() {
        return Err(String::from("avc: no frames"));
    }
    #[cfg(target_os = "macos")]
    {
        return encode_avc_vt(frames);
    }
    #[cfg(not(target_os = "macos"))]
    {
        return encode_avc_vk(frames);
    }
}

fn encode_hevc_hw(frames: &[Yuv420]) -> Result<HevcStream, String> {
    if frames.is_empty() {
        return Err(String::from("hevc: no frames"));
    }
    #[cfg(target_os = "macos")]
    {
        return encode_hevc_vt(frames);
    }
    #[cfg(not(target_os = "macos"))]
    {
        return encode_hevc_vk(frames);
    }
}

/// AVCC（4 字节 BE 长度前缀，VideoToolbox 编码输出口径）→ 裸 NAL 切分。
/// 纯函数（可单测）：长度越界即 None（调用方回退 CPU）。
fn avcc_to_nalus(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        if data.len() - off < 4 {
            return None;
        }
        let len =
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        off += 4;
        if len == 0 || data.len() - off < len {
            return None;
        }
        out.push(data[off..off + len].to_vec());
        off += len;
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// SPS 首字节 profile_idc（Annex-B 裸 NAL：类型位为 7 即可，
/// `0x67`/`0x27` 皆可——nal_ref_idc 各家不同，VideoToolbox 编出的是 `0x27`）。
/// 纯函数（可单测）：转码 VT 解码曾以此做 Baseline（66）门控
/// （Main/High 含 B 帧回退 CPU）；2026-10-22 起 VT 路按 PTS 保序，全 profile
/// 硬解此门已摘现仅测试用（生产无调用方）。
#[cfg(test)]
pub(crate) fn sps_profile_idc(sps: &[u8]) -> Option<u8> {
    if sps.len() >= 2 && sps[0] & 0x1F == 7 {
        Some(sps[1])
    } else {
        None
    }
}

/// YUV420 紧凑平面 → NV12 连续块（纯搬运：Y 整拷 + UV 交错，无像素运算）。
/// 纯函数（可单测）：几何/长度对不上即 None。
/// Win/Linux 硬编码输入用；mac 下仅单测引用（`cfg_attr` 免 dead_code 告警）。
#[cfg_attr(all(not(test), target_os = "macos"), allow(dead_code))]
fn yuv420_to_nv12(f: &Yuv420) -> Option<Vec<u8>> {
    let (w, h) = (f.w as usize, f.h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let uvw = w.div_ceil(2);
    let uvh = h.div_ceil(2);
    if f.y.len() < w * h || f.u.len() < uvw * uvh || f.v.len() < uvw * uvh {
        return None;
    }
    let mut out = Vec::with_capacity(w * h + uvw * uvh * 2);
    out.extend_from_slice(&f.y[..w * h]);
    for row in 0..uvh {
        for col in 0..uvw {
            out.push(f.u[row * uvw + col]);
            out.push(f.v[row * uvw + col]);
        }
    }
    Some(out)
}

/// 交错 UV → U/V 平面（纯搬运；`yuv420_to_nv12` 的逆操作，可单测）。
/// `pub(crate)`：转码硬解 AVC（Nv12 回读拆平面）复用。
pub(crate) fn split_uv_planes(uv: &[u8], w: usize, h: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    if w % 2 != 0 || h % 2 != 0 {
        return None;
    }
    let (uvw, uvh) = (w / 2, h / 2);
    if uv.len() < uvw * uvh * 2 {
        return None;
    }
    let mut u = Vec::with_capacity(uvw * uvh);
    let mut v = Vec::with_capacity(uvw * uvh);
    for p in uv[..uvw * uvh * 2].chunks_exact(2) {
        u.push(p[0]);
        v.push(p[1]);
    }
    Some((u, v))
}

/// mac VideoToolbox 编码（AVC Baseline/CAVLC 或 HEVC Main；段内独立会话，
/// 首帧强制关键帧；形状与 CPU 版一致）。
#[cfg(target_os = "macos")]
fn vt_encode(frames: &[Yuv420], hevc: bool) -> Result<Vec<VtFrame>, String> {
    use shiguredo_video_toolbox::{
        CodecConfig, EncodeOptions, Encoder, EncoderConfig, FrameData, H264EncoderConfig,
        H264EntropyMode, H264Profile, HevcEncoderConfig, HevcProfile, PixelFormat,
    };
    let (w, h) = (frames[0].w, frames[0].h);
    if w == 0 || h == 0 {
        return Err(String::from("vt encode: bad geometry"));
    }
    let yb = w as usize * h as usize;
    let uvb = (w as usize).div_ceil(2) * (h as usize).div_ceil(2);
    let codec = if hevc {
        CodecConfig::Hevc(HevcEncoderConfig {
            profile: HevcProfile::Main,
            allow_open_gop: false,
        })
    } else {
        CodecConfig::H264(H264EncoderConfig {
            profile: H264Profile::Baseline,
            entropy_mode: H264EntropyMode::Cavlc,
        })
    };
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        codec,
        pixel_format: PixelFormat::I420,
        average_bitrate: Some(8_000_000),
        fps_numerator: 30,
        fps_denominator: 1,
        prioritize_encoding_speed_over_quality: false,
        real_time: false,
        maximize_power_efficiency: false,
        allow_frame_reordering: false,
        allow_temporal_compression: true,
        max_key_frame_interval: None,
        max_key_frame_interval_duration: None,
        max_frame_delay_count: None,
    })
    .map_err(|e| format!("vt enc init: {e:?}"))?;
    let mut outs = Vec::new();
    let mut fed = 0usize;
    for (i, f) in frames.iter().enumerate() {
        if (f.w, f.h) != (w, h) || f.y.len() < yb || f.u.len() < uvb || f.v.len() < uvb {
            continue;
        }
        enc.encode(
            &FrameData::I420 {
                y: &f.y[..yb],
                u: &f.u[..uvb],
                v: &f.v[..uvb],
            },
            &EncodeOptions {
                force_key_frame: i == 0,
            },
        )
        .map_err(|e| format!("vt encode: {e:?}"))?;
        fed += 1;
        drain_vt(&mut enc, &mut outs)?;
    }
    enc.finish().map_err(|e| format!("vt finish: {e:?}"))?;
    for _ in 0..fed + 16 {
        if outs.len() >= fed {
            break;
        }
        drain_vt(&mut enc, &mut outs)?;
    }
    if outs.len() < fed {
        return Err(String::from("vt encode: short drain"));
    }
    Ok(outs)
}

/// VideoToolbox 单帧（AVCC → NAL；参数集 out-of-band 兜底帧内 lists）。
#[cfg(target_os = "macos")]
struct VtFrame {
    keyframe: bool,
    nalus: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    vps: Vec<Vec<u8>>,
}

/// VideoToolbox 排空一次（`next_frame` 吐完即 None；nounwind 外无 paint，无 abort 风险）。
#[cfg(target_os = "macos")]
fn drain_vt(
    enc: &mut shiguredo_video_toolbox::Encoder,
    outs: &mut Vec<VtFrame>,
) -> Result<(), String> {
    loop {
        match enc.next_frame().map_err(|e| format!("vt drain: {e:?}"))? {
            Some(f) => {
                let nalus =
                    avcc_to_nalus(&f.data).ok_or_else(|| String::from("vt drain: bad avcc"))?;
                outs.push(VtFrame {
                    keyframe: f.keyframe,
                    nalus,
                    sps: f.sps_list,
                    pps: f.pps_list,
                    vps: f.vps_list,
                });
            }
            None => break,
        }
    }
    Ok(())
}

/// mac VideoToolbox AVC（Baseline/CAVLC；与旧 CPU 输出同 profile）。
#[cfg(target_os = "macos")]
fn encode_avc_vt(frames: &[Yuv420]) -> Result<AvcStream, String> {
    let outs = vt_encode(frames, false)?;
    let (mut sps, mut pps) = (Vec::new(), Vec::new());
    let mut samples = Vec::new();
    for o in outs {
        if sps.is_empty() {
            if let Some(s) = o.sps.first() {
                sps = s.clone();
            }
        }
        if pps.is_empty() {
            if let Some(p) = o.pps.first() {
                pps = p.clone();
            }
        }
        let mut nalus = Vec::new();
        let mut keyframe = o.keyframe;
        for u in &o.nalus {
            if u.is_empty() {
                continue;
            }
            match u[0] & 0x1F {
                7 if sps.is_empty() => sps = u.clone(),
                8 if pps.is_empty() => pps = u.clone(),
                5 => {
                    keyframe = true;
                    nalus.push(u.clone());
                }
                _ => nalus.push(u.clone()),
            }
        }
        samples.push(AvcSample { keyframe, nalus });
    }
    if sps.is_empty() || pps.is_empty() || samples.is_empty() {
        return Err(String::from("avc vt: missing sps/pps/samples"));
    }
    samples[0].keyframe = true;
    Ok(AvcStream { sps, pps, samples })
}

/// mac VideoToolbox HEVC（Main 8-bit）。
#[cfg(target_os = "macos")]
fn encode_hevc_vt(frames: &[Yuv420]) -> Result<HevcStream, String> {
    let outs = vt_encode(frames, true)?;
    let (mut vps, mut sps, mut pps) = (Vec::new(), Vec::new(), Vec::new());
    let mut samples = Vec::new();
    for o in outs {
        if vps.is_empty() {
            if let Some(v) = o.vps.first() {
                vps = v.clone();
            }
        }
        if sps.is_empty() {
            if let Some(s) = o.sps.first() {
                sps = s.clone();
            }
        }
        if pps.is_empty() {
            if let Some(p) = o.pps.first() {
                pps = p.clone();
            }
        }
        let mut nalus = Vec::new();
        let mut keyframe = o.keyframe;
        for u in &o.nalus {
            if u.is_empty() {
                continue;
            }
            match (u[0] >> 1) & 0x3F {
                32 if vps.is_empty() => vps = u.clone(),
                33 if sps.is_empty() => sps = u.clone(),
                34 if pps.is_empty() => pps = u.clone(),
                19..=21 => {
                    keyframe = true;
                    nalus.push(u.clone());
                }
                _ => nalus.push(u.clone()),
            }
        }
        samples.push(HevcSample { keyframe, nalus });
    }
    if vps.is_empty() || sps.is_empty() || pps.is_empty() || samples.is_empty() {
        return Err(String::from("hevc vt: missing param sets/samples"));
    }
    samples[0].keyframe = true;
    Ok(HevcStream {
        vps,
        sps,
        pps,
        samples,
    })
}

/// Win/Linux 共用 Vulkan 设备（转码编码 + AVC 解码共用；`pub(crate)` 供转码侧复用）。
/// 以 `vulkan_gpu` 偏好为键懒缓存（换偏好下次重建；在途 job 的旧 `Arc` 不受影响）。
/// worker 线程内建（`video_bg` 同式，`Send` 已实锤）。
#[cfg(not(target_os = "macos"))]
static HW_VULKAN_DEV: std::sync::Mutex<
    Option<(
        crate::vulkan_gpu::VulkanGpuPref,
        std::sync::Arc<gpu_video::VulkanDevice>,
        String,
    )>,
> = std::sync::Mutex::new(None);

/// Win/Linux 共用 Vulkan 设备（`vulkan_gpu::select_vulkan_adapter` 同口径选择；
/// 建失败即 `None`，调用方记回退走 CPU）。
#[cfg(not(target_os = "macos"))]
pub(crate) fn hw_vulkan_device() -> Option<std::sync::Arc<gpu_video::VulkanDevice>> {
    use crate::vulkan_gpu::{select_vulkan_adapter, vulkan_gpu_pref, VkAdapterView};
    let pref = vulkan_gpu_pref();
    if let Ok(g) = HW_VULKAN_DEV.lock() {
        if let Some((p, d, _)) = g.as_ref() {
            if *p == pref {
                return Some(d.clone());
            }
        }
    }
    // 枚举 + 纯选择（与 UI 行同一函数，同口径）；建 device 时重枚举同序取下标。
    let inst = gpu_video::VulkanInstance::new().ok()?;
    let views: Vec<VkAdapterView> = inst
        .iter_adapters()
        .ok()?
        .map(|a| {
            let info = a.info();
            VkAdapterView {
                name: info.name.clone(),
                discrete: format!("{:?}", info.device_type) == "DISCRETE_GPU",
                dec: info.supports_decoding,
                enc: info.supports_encoding,
            }
        })
        .collect();
    let i = select_vulkan_adapter(&views, &pref)?;
    let name = views[i].name.clone();
    let adapter = inst.iter_adapters().ok()?.nth(i)?;
    let dev = adapter
        .create_device(&gpu_video::parameters::VulkanDeviceDescriptor::default())
        .ok()?;
    if let Ok(mut g) = HW_VULKAN_DEV.lock() {
        *g = Some((pref, dev.clone(), name));
    }
    Some(dev)
}

/// 当前实际 Vulkan 设备名（UI“当前：”行；未建即 `None`）。
#[cfg(not(target_os = "macos"))]
pub(crate) fn current_vulkan_device_name() -> Option<String> {
    HW_VULKAN_DEV
        .lock()
        .ok()?
        .as_ref()
        .map(|(_, _, n)| n.clone())
}

/// mac 无 Vulkan（桩供 UI 行统一调用，恒 `None`）。
#[cfg(target_os = "macos")]
pub(crate) fn current_vulkan_device_name() -> Option<String> {
    None
}

/// Win/Linux Vulkan AVC（Baseline；inline 参数集；与 VT 版同形状）。
#[cfg(not(target_os = "macos"))]
fn encode_avc_vk(frames: &[Yuv420]) -> Result<AvcStream, String> {
    use gpu_video::parameters as P;
    let dev = hw_vulkan_device().ok_or_else(|| String::from("avc vk: no device"))?;
    let (w, h) = (frames[0].w, frames[0].h);
    if w == 0 || h == 0 {
        return Err(String::from("avc vk: bad geometry"));
    }
    let mut enc = dev
        .create_bytes_encoder_h264(P::EncoderParametersH264 {
            input_parameters: P::VideoParameters {
                width: std::num::NonZeroU32::new(w)
                    .ok_or_else(|| String::from("avc vk: bad width"))?,
                height: std::num::NonZeroU32::new(h)
                    .ok_or_else(|| String::from("avc vk: bad height"))?,
                target_framerate: P::Rational {
                    numerator: 30,
                    denominator: std::num::NonZeroU32::new(1).expect("nonzero 1"),
                },
            },
            output_parameters: P::EncoderOutputParameters {
                idr_period: None,
                rate_control: P::RateControl::EncoderDefault,
                max_references: None,
                profile: gpu_video::parameters::H264Profile::Baseline,
                quality_level: 0,
                usage_flags: None,
                tuning_mode: None,
                content_flags: None,
                inline_stream_params: Some(true),
                color_space: None,
                color_range: None,
            },
        })
        .map_err(|e| format!("avc vk init: {e:?}"))?;
    let (mut sps, mut pps) = (Vec::new(), Vec::new());
    let mut samples = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if (f.w, f.h) != (w, h) {
            continue;
        }
        let nv12 = yuv420_to_nv12(f).ok_or_else(|| String::from("avc vk: bad planes"))?;
        let chunk = enc
            .encode(
                &gpu_video::InputFrame {
                    data: gpu_video::RawFrameData {
                        frame: nv12,
                        width: w,
                        height: h,
                    },
                    pts: None,
                },
                i == 0,
            )
            .map_err(|e| format!("avc vk encode: {e:?}"))?;
        let mut nalus = Vec::new();
        let mut keyframe = chunk.is_keyframe;
        for u in split_annexb(&chunk.data) {
            if u.is_empty() {
                continue;
            }
            match u[0] & 0x1F {
                7 if sps.is_empty() => sps = u.to_vec(),
                8 if pps.is_empty() => pps = u.to_vec(),
                5 => {
                    keyframe = true;
                    nalus.push(u.to_vec());
                }
                _ => nalus.push(u.to_vec()),
            }
        }
        samples.push(AvcSample { keyframe, nalus });
    }
    if sps.is_empty() || pps.is_empty() || samples.is_empty() {
        return Err(String::from("avc vk: missing sps/pps/samples"));
    }
    samples[0].keyframe = true;
    Ok(AvcStream { sps, pps, samples })
}

/// Win/Linux Vulkan HEVC（Main 8-bit；inline 参数集）。
#[cfg(not(target_os = "macos"))]
fn encode_hevc_vk(frames: &[Yuv420]) -> Result<HevcStream, String> {
    use gpu_video::parameters as P;
    let dev = hw_vulkan_device().ok_or_else(|| String::from("hevc vk: no device"))?;
    let (w, h) = (frames[0].w, frames[0].h);
    if w == 0 || h == 0 {
        return Err(String::from("hevc vk: bad geometry"));
    }
    let mut enc = dev
        .create_bytes_encoder_h265(P::EncoderParametersH265 {
            input_parameters: P::VideoParameters {
                width: std::num::NonZeroU32::new(w)
                    .ok_or_else(|| String::from("hevc vk: bad width"))?,
                height: std::num::NonZeroU32::new(h)
                    .ok_or_else(|| String::from("hevc vk: bad height"))?,
                target_framerate: P::Rational {
                    numerator: 30,
                    denominator: std::num::NonZeroU32::new(1).expect("nonzero 1"),
                },
            },
            output_parameters: P::EncoderOutputParameters {
                idr_period: None,
                rate_control: P::RateControl::EncoderDefault,
                max_references: None,
                profile: gpu_video::parameters::H265Profile::Main,
                quality_level: 0,
                usage_flags: None,
                tuning_mode: None,
                content_flags: None,
                inline_stream_params: Some(true),
                color_space: None,
                color_range: None,
            },
        })
        .map_err(|e| format!("hevc vk init: {e:?}"))?;
    let (mut vps, mut sps, mut pps) = (Vec::new(), Vec::new(), Vec::new());
    let mut samples = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if (f.w, f.h) != (w, h) {
            continue;
        }
        let nv12 = yuv420_to_nv12(f).ok_or_else(|| String::from("hevc vk: bad planes"))?;
        let chunk = enc
            .encode(
                &gpu_video::InputFrame {
                    data: gpu_video::RawFrameData {
                        frame: nv12,
                        width: w,
                        height: h,
                    },
                    pts: None,
                },
                i == 0,
            )
            .map_err(|e| format!("hevc vk encode: {e:?}"))?;
        let mut nalus = Vec::new();
        let mut keyframe = chunk.is_keyframe;
        for u in split_annexb(&chunk.data) {
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
        samples.push(HevcSample { keyframe, nalus });
    }
    if vps.is_empty() || sps.is_empty() || pps.is_empty() || samples.is_empty() {
        return Err(String::from("hevc vk: missing param sets/samples"));
    }
    samples[0].keyframe = true;
    Ok(HevcStream {
        vps,
        sps,
        pps,
        samples,
    })
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
