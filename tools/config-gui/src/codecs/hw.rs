//! 硬件编码后端（mac VideoToolbox / Win-Linux Vulkan Video + 回退标记；拆自 `codecs.rs`，行为零改）。

use super::*;

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
pub(crate) fn encode_avc_hw(frames: &[Yuv420]) -> Result<AvcStream, String> {
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

pub(crate) fn encode_hevc_hw(frames: &[Yuv420]) -> Result<HevcStream, String> {
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
pub(crate) fn avcc_to_nalus(data: &[u8]) -> Option<Vec<Vec<u8>>> {
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
pub(crate) fn yuv420_to_nv12(f: &Yuv420) -> Option<Vec<u8>> {
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
pub(crate) fn vt_encode(frames: &[Yuv420], hevc: bool) -> Result<Vec<VtFrame>, String> {
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
pub(crate) struct VtFrame {
    keyframe: bool,
    nalus: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    vps: Vec<Vec<u8>>,
}

/// VideoToolbox 排空一次（`next_frame` 吐完即 None；nounwind 外无 paint，无 abort 风险）。
#[cfg(target_os = "macos")]
pub(crate) fn drain_vt(
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
pub(crate) fn encode_avc_vt(frames: &[Yuv420]) -> Result<AvcStream, String> {
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
pub(crate) fn encode_hevc_vt(frames: &[Yuv420]) -> Result<HevcStream, String> {
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
pub(crate) static HW_VULKAN_DEV: std::sync::Mutex<
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
pub(crate) fn encode_avc_vk(frames: &[Yuv420]) -> Result<AvcStream, String> {
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
pub(crate) fn encode_hevc_vk(frames: &[Yuv420]) -> Result<HevcStream, String> {
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
