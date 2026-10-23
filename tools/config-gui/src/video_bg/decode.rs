//! 动态背景后台解码循环（mac VideoToolbox / 其他平台 CPU；拆自 `video_bg.rs`，行为零改）。

use super::*;

/// 后台解码循环（worker 线程；阻塞式，背压由 `sync_channel` 承担）。
/// 只传 NV12 平面（无 BT.601、无 RGBA，CPU 零像素运算）；失败只发一次
/// `Finished(Err)`，UI 静默回退（用户拍板，不弹窗）。
#[cfg(target_os = "macos")]
pub(crate) fn decode_loop(path: &str, tx: SyncSender<BgMsg>) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg read: {e}"))));
            return;
        }
    };
    decode_bytes(data, tx)
}

/// 内存解码循环（内嵌默认背景走此路；与 `decode_loop` 同链，只跳过读盘）。
#[cfg(target_os = "macos")]
pub(crate) fn decode_bytes(data: Vec<u8>, tx: SyncSender<BgMsg>) {
    use crate::schema::MediaFormat;
    use crate::transcode::demux_groups;

    // 只收 AVC 轨优先无则 HEVC（选中门保证至少其一；vp9 永不进本函数）。
    let (demuxed, hevc) = match demux_groups(&data, MediaFormat::Avc, None) {
        Ok(d) => (d, false),
        Err(_) => match demux_groups(&data, MediaFormat::Hevc, None) {
            Ok(d) => (d, true),
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(format!("video bg demux: {e}"))));
                return;
            }
        },
    };
    let fps = demuxed.fps.clamp(1, 120);
    let _ = tx.send(BgMsg::Ready {
        w: 1280,
        h: 720,
        fps,
    });
    // 外层即整片回绕：SPS/PPS 固化一次，每轮重建硬解器（干净 IDR，无跨轮参考）。
    let params = if hevc {
        match split_hevc_param_sets(&demuxed.annexb_prefix) {
            Some((v, sv, p)) => BgParams::Hevc {
                vps: Box::leak(v.into_boxed_slice()),
                sps: Box::leak(sv.into_boxed_slice()),
                pps: Box::leak(p.into_boxed_slice()),
            },
            None => {
                let _ = tx.send(BgMsg::Finished(Err(String::from(
                    "video bg: no vps/sps/pps",
                ))));
                return;
            }
        }
    } else {
        let units = crate::codecs::split_annexb(&demuxed.annexb_prefix);
        if units.len() < 2 {
            let _ = tx.send(BgMsg::Finished(Err(String::from("video bg: no sps/pps"))));
            return;
        }
        BgParams::Avc {
            sps: Box::leak(units[0].to_vec().into_boxed_slice()),
            pps: Box::leak(units[1].to_vec().into_boxed_slice()),
        }
    };
    let nalu_len = demuxed.length_size as u32;
    loop {
        let wall_start = std::time::Instant::now();
        let dec = match &params {
            BgParams::Avc { sps, pps } => make_decoder(sps, pps, nalu_len),
            BgParams::Hevc { vps, sps, pps } => make_decoder_hevc(vps, sps, pps, nalu_len),
        };
        let mut dec = match dec {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(e)));
                return;
            }
        };
        let mut elapsed = 0.0f64;
        // B 帧重排窗：VT 按解码序吐帧（High 档 P,P,B,B,B…），攒够按 pts 取最小
        // 发出（显示序）；窗满才吐，深度盖连 B，不断流、不攒整组。
        let mut pending: Vec<(i128, (u32, u32, Vec<u8>, Vec<u8>))> = Vec::new();
        // 首组前缀（SPS/PPS）VideoToolbox 建会话时已消费，此处只喂样片负载
        // （length-prefixed AVCC，与 `nalu_len_bytes` 一致，无需转 Annex-B）。
        for g in &demuxed.groups {
            for r in g {
                elapsed += r.dur;
                match dec.decode(&r.payload) {
                    Ok(Some(frame)) => {
                        if let Some((w, h, y, uv)) = frame_to_nv12(&frame) {
                            pending.push((r.pts, (w, h, y, uv)));
                        }
                        // `None`（隐藏帧/重排等待）不计帧，继续喂。
                        while pending.len() > REORDER_DEPTH {
                            if let Some((_, (w, h, y, uv))) = pop_earliest(&mut pending) {
                                // 背压：UI 跟不上即阻塞在此，不堆内存。
                                if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
                                    return;
                                }
                                let wall_secs = wall_start.elapsed().as_secs_f64();
                                let sleep_ms = VideoBg::pace_sleep_ms(elapsed, wall_secs);
                                if sleep_ms > 0 {
                                    std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // 坏帧跳过（装饰不 halt）。
                        let _ = e;
                        continue;
                    }
                }
            }
        }
        // 整片播完排空窗口（显示序），再回绕。
        while let Some((_, (w, h, y, uv))) = pop_earliest(&mut pending) {
            if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
                return;
            }
            let wall_secs = wall_start.elapsed().as_secs_f64();
            let sleep_ms = VideoBg::pace_sleep_ms(elapsed, wall_secs);
            if sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
        }
        // 整片播完即回绕（从头再播，同 loop 语义）。
    }
}

/// VideoToolbox 解码器构造（SPS/PPS/length_size 直给会话；NV12 输出，CPU 只搬运）。
#[cfg(target_os = "macos")]
/// 背景解码参数集（Avc/Hevc 会话二选一；`'static` 泄漏固化进程级常驻，
/// 整片回绕重建会话只借用不重复泄漏）。
#[cfg(target_os = "macos")]
pub(crate) enum BgParams {
    Avc {
        sps: &'static [u8],
        pps: &'static [u8],
    },
    Hevc {
        vps: &'static [u8],
        sps: &'static [u8],
        pps: &'static [u8],
    },
}

/// HEVC 参数集切分（纯函数可单测）：Annex-B 前缀 → VPS(32)/SPS(33)/PPS(34)。
/// 缺任一即 None（调用方 Finished(Err) 静默回退）。
pub(crate) fn split_hevc_param_sets(prefix: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let (mut vps, mut sps, mut pps) = (None, None, None);
    for u in crate::codecs::split_annexb(prefix) {
        if u.is_empty() {
            continue;
        }
        match (u[0] >> 1) & 0x3F {
            32 if vps.is_none() => vps = Some(u.to_vec()),
            33 if sps.is_none() => sps = Some(u.to_vec()),
            34 if pps.is_none() => pps = Some(u.to_vec()),
            _ => {}
        }
    }
    match (vps, sps, pps) {
        (Some(v), Some(sp), Some(pp)) => Some((v, sp, pp)),
        _ => None,
    }
}

/// VideoToolbox 解码器构造（SPS/PPS/length_size 直给会话；NV12 输出，CPU 只搬运）。
#[cfg(target_os = "macos")]
pub(crate) fn make_decoder(
    sps: &'static [u8],
    pps: &'static [u8],
    nalu_len: u32,
) -> Result<shiguredo_video_toolbox::Decoder, String> {
    use shiguredo_video_toolbox::{Decoder, DecoderCodec, DecoderConfig, PixelFormat};
    Decoder::new(DecoderConfig {
        codec: DecoderCodec::H264 {
            sps,
            pps,
            nalu_len_bytes: nalu_len,
        },
        pixel_format: PixelFormat::Nv12,
    })
    .map_err(|e| format!("video bg decoder: {e:?}"))
}

/// VideoToolbox HEVC 解码器构造（VPS/SPS/PPS/length_size 直给会话；余同 AVC）。
#[cfg(target_os = "macos")]
pub(crate) fn make_decoder_hevc(
    vps: &'static [u8],
    sps: &'static [u8],
    pps: &'static [u8],
    nalu_len: u32,
) -> Result<shiguredo_video_toolbox::Decoder, String> {
    use shiguredo_video_toolbox::{Decoder, DecoderCodec, DecoderConfig, PixelFormat};
    Decoder::new(DecoderConfig {
        codec: DecoderCodec::Hevc {
            vps,
            sps,
            pps,
            nalu_len_bytes: nalu_len,
        },
        pixel_format: PixelFormat::Nv12,
    })
    .map_err(|e| format!("video bg hevc decoder: {e:?}"))
}

/// Nv12 帧（stride 可能 > 宽）→ 紧凑 Y + 交错 UV（纯搬运，无像素运算）。
/// 空平面（异常哨兵，见上游文档）即跳过该帧（返回 None，不断流）。
#[cfg(target_os = "macos")]
pub(crate) fn frame_to_nv12(
    frame: &shiguredo_video_toolbox::DecodedFrame<'_>,
) -> Option<(u32, u32, Vec<u8>, Vec<u8>)> {
    match frame {
        shiguredo_video_toolbox::DecodedFrame::Nv12(f) => {
            let (w, h) = (f.width(), f.height());
            if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
                return None;
            }
            let (yp, uvp) = (f.y_plane(), f.uv_plane());
            if yp.is_empty() || uvp.is_empty() {
                return None;
            }
            let (ys, uvs) = (f.y_stride(), f.uv_stride());
            let y = pack_plane(yp, ys, w, h)?;
            let uv = pack_plane(uvp, uvs, w, h / 2)?;
            Some((w as u32, h as u32, y, uv))
        }
        shiguredo_video_toolbox::DecodedFrame::I420(_) => {
            // 本背景只开 Nv12 会话，走不到此分支（穷尽匹配保留）。
            None
        }
    }
}

/// Win/Linux 硬解循环（Vulkan Video H.264，`gpu-video` `BytesDecoder` NV12 回读）。
/// 只拆 Y/UV（memcpy），不做 BT.601；任一步硬解不可用即 `Finished(Err)`
/// 静默回退，绝不回退 CPU 软解（用户拍板：GUI 内零软解）。
#[cfg(not(target_os = "macos"))]
pub(crate) fn decode_loop(path: &str, tx: SyncSender<BgMsg>) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg read: {e}"))));
            return;
        }
    };
    decode_bytes(data, tx)
}

/// 内存解码循环（内嵌默认背景走此路；与 `decode_loop` 同链，只跳过读盘）。
#[cfg(not(target_os = "macos"))]
pub(crate) fn decode_bytes(data: Vec<u8>, tx: SyncSender<BgMsg>) {
    use crate::codecs::length_prefixed_to_annexb;
    use crate::schema::MediaFormat;
    use crate::transcode::demux_groups;

    // 只收 AVC 轨（`gpu-video` 解码侧只支持 H.264，见上游兼容表）。
    let demuxed = match demux_groups(&data, MediaFormat::Avc, None) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg demux: {e}"))));
            return;
        }
    };
    let fps = demuxed.fps.clamp(1, 120);
    let _ = tx.send(BgMsg::Ready {
        w: 1280,
        h: 720,
        fps,
    });
    // Vulkan 硬解设备只建一次（整片回绕只重建 `BytesDecoder`，device 复用）。
    let instance = match gpu_video::VulkanInstance::new() {
        Ok(i) => i,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg vulkan: {e:?}"))));
            return;
        }
    };
    let adapter = match instance.create_adapter(&gpu_video::parameters::VulkanAdapterDescriptor {
        supports_decoding: true,
        supports_encoding: false,
    }) {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg adapter: {e:?}"))));
            return;
        }
    };
    let device =
        match adapter.create_device(&gpu_video::parameters::VulkanDeviceDescriptor::default()) {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(format!("video bg device: {e:?}"))));
                return;
            }
        };
    // 外层即整片回绕：每轮重建 `BytesDecoder`（干净 IDR，无跨轮参考）。
    loop {
        let wall_start = std::time::Instant::now();
        let mut dec = match device
            .create_bytes_decoder_h264(gpu_video::parameters::DecoderParameters::default())
        {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(format!("video bg decoder: {e:?}"))));
                return;
            }
        };
        // 首包带 SPS/PPS 前缀（`gpu-video` 吃 Annex-B bytestream，见上游 player 示例；
        // 本仓 `length_size` 前缀先转 Annex-B，与固件 `demux_mp4_track` 同式）。
        // B 帧重排窗：同 VT 路，攒够按 pts 取最小发出（显示序；同输入多帧同 pts
        // 按解码序，`pop_earliest` 稳定）。
        let mut first = true;
        let mut elapsed = 0.0f64;
        let mut pending: Vec<(i128, (u32, u32, Vec<u8>, Vec<u8>))> = Vec::new();
        for g in &demuxed.groups {
            for r in g {
                elapsed += r.dur;
                let mut annexb = Vec::new();
                if first {
                    annexb.extend_from_slice(&demuxed.annexb_prefix);
                    first = false;
                }
                annexb.extend(length_prefixed_to_annexb(&r.payload, demuxed.length_size));
                match dec.decode(gpu_video::EncodedInputChunk {
                    data: &annexb,
                    pts: None,
                }) {
                    Ok(frames) => {
                        for f in &frames {
                            if let Some((w, h, y, uv)) =
                                split_nv12(&f.data.frame, f.data.width, f.data.height)
                            {
                                pending.push((r.pts, (w, h, y, uv)));
                            }
                        }
                        while pending.len() > REORDER_DEPTH {
                            if let Some((_, (w, h, y, uv))) = pop_earliest(&mut pending) {
                                if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
                                    return;
                                }
                                let wall_secs = wall_start.elapsed().as_secs_f64();
                                let sleep_ms = VideoBg::pace_sleep_ms(elapsed, wall_secs);
                                if sleep_ms > 0 {
                                    std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // 坏块跳过（装饰不 halt）。
                        let _ = e;
                        continue;
                    }
                }
            }
        }
        // 整片播完排空窗口（显示序）再回绕（`flush` 把重排延迟帧吐完，不丢尾）。
        while let Some((_, (w, h, y, uv))) = pop_earliest(&mut pending) {
            if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
                return;
            }
            let wall_secs = wall_start.elapsed().as_secs_f64();
            let sleep_ms = VideoBg::pace_sleep_ms(elapsed, wall_secs);
            if sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
        }
    }
}

/// 紧凑 NV12（Y 整幅 + UV 交错半幅）→ 拆 Y/UV（纯搬运； stride 即宽，无需打包）。
#[cfg(not(target_os = "macos"))]
pub(crate) fn split_nv12(frame: &[u8], w: u32, h: u32) -> Option<(u32, u32, Vec<u8>, Vec<u8>)> {
    if w < 16 || h < 16 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    if frame.len() < w * h * 3 / 2 {
        return None;
    }
    Some((
        w as u32,
        h as u32,
        frame[..w * h].to_vec(),
        frame[w * h..w * h * 3 / 2].to_vec(),
    ))
}

/// 带 stride 平面 → 紧凑行（逐行 memcpy，无像素运算；越界即 None，不断流）。
/// 仅 mac VideoToolbox 路用（Win/Linux 的 NV12 已紧凑），门控避他平台死码警告。
#[cfg(target_os = "macos")]
pub(crate) fn pack_plane(src: &[u8], stride: usize, w: usize, h: usize) -> Option<Vec<u8>> {
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
