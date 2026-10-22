//! AVC→AVC 硬解/回退路径测试（转码页，host `std`）。
//!
//! 背景（2026-10-22 用户实问“avc转avc是不是用的cpu”）：mac 上 AVC 编解码
//! 都是硬解优先、失败回退 CPU（见 `codecs.rs` 头注释 + `transcode.rs`
//! `TrackDecoder::decode_avc_vt`）：
//! - 编码 `encode_avc`：VideoToolbox Baseline/CAVLC，失败才 `less-avc` CPU。
//! - 解码 `decode_group(Avc)`：全 profile 走 VideoToolbox Nv12，B 帧按 PTS
//!   排回显示序（2026-10-22 前仅 Baseline 走硬解，High 回退 CPU；scratch 探针
//!   实锤 VT 同步解码 1:1 吐解码序后摘门，见 `decode_avc_vt` 头注释）。
//! - `JobOut.hw_fallback` 是编解码任一回退的 sticky 位（UI 追加 `[CPU]`
//!   后缀 + toast，见 `poll_transcode_job`）；任一为 CPU 即整 job 标 CPU。
//!
//! 轮子结论：不新增轮子。Baseline 源复用 `encode_avc` 自产 +
//! `shiguredo_mp4::Mp4FileMuxer` 打包（与生产 `EncodeAcc::finish` 同形状），
//! 经 `demux_groups` + `decode_group` 走生产解码分支；High 含 B 帧料复用固件侧
//! 现货小样片（`src/diag/data/video-avc.mp4`，ffmpeg 制自带 ctts），
//! Hevc/Vp9 解码仍纯 CPU 不在此文件测。
//! 文件位置：`transcode.rs`（1800+ 行）/`codecs.rs`（1550+ 行）均已超
//! 1000 行口径（存量超限），故新测试独立成文件，不继续往里堆。

use crate::codecs::{encode_avc, note_hw_fallback, sps_profile_idc, take_hw_fallback_hit, Yuv420};
use crate::schema::MediaFormat;
use crate::transcode::{
    avc_entry, decode_group, demux_groups, length_prefixed_nals, mux_mp4_video, Mp4OutSample,
    TrackDecoder,
};

fn solid_yuv(w: u32, h: u32, y: u8) -> Yuv420 {
    Yuv420 {
        w,
        h,
        y: vec![y; (w * h) as usize],
        u: vec![128u8; (w.div_ceil(2) * h.div_ceil(2)) as usize],
        v: vec![128u8; (w.div_ceil(2) * h.div_ceil(2)) as usize],
    }
}

/// AVC 编码输出是 Baseline 且 mac 上走硬解（无回退）。
///
/// 断言口径：`encode_avc`（生产入口，HW 优先）产出 SPS `profile_idc == 66`；
/// mac 上 `take_hw_fallback_hit() == false` 即全程无回退＝硬解编码。
/// 非 mac（Vulkan 驱动缺即回退 CPU）不断言回退位只锁输出形状。
#[test]
fn avc_encode_is_baseline_and_hw_on_mac() {
    let _ = take_hw_fallback_hit();
    let frames = vec![solid_yuv(64, 48, 16), solid_yuv(64, 48, 235)];
    let st = encode_avc(&frames).expect("avc encode");
    assert_eq!(
        sps_profile_idc(&st.sps),
        Some(66),
        "avc encoder must stay Baseline (66), got {:?}",
        sps_profile_idc(&st.sps)
    );
    assert!(!st.pps.is_empty());
    assert_eq!(st.samples.len(), 2);
    assert!(st.samples[0].keyframe);
    #[cfg(target_os = "macos")]
    assert!(
        !take_hw_fallback_hit(),
        "mac VT avc encode should not fall back to CPU"
    );
    #[cfg(not(target_os = "macos"))]
    let _ = take_hw_fallback_hit();
}

/// Baseline 源 AVC→AVC 全硬解；回退位语义锁死。
///
/// 用 `encode_avc` 自产 Baseline → mux → `demux_groups` → `decode_group`，
/// mac 上编解码零回退（＝VideoToolbox Nv12 路真跑了），帧数对得上。
#[test]
fn avc_baseline_decodes_hw_no_fallback() {
    // 自产 Baseline mp4（与 `EncodeAcc::finish(Avc)` 同形状）。
    let frames = vec![solid_yuv(64, 48, 16), solid_yuv(64, 48, 235)];
    let st = encode_avc(&frames).expect("avc encode");
    let entry = avc_entry(&st.sps, &st.pps, 64, 48);
    let samples: Vec<Mp4OutSample> = st
        .samples
        .iter()
        .map(|x| Mp4OutSample {
            keyframe: x.keyframe,
            payload: length_prefixed_nals(&x.nalus),
        })
        .collect();
    let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");

    // 生产解复用 + 生产解码分支。
    let _ = take_hw_fallback_hit();
    let d = demux_groups(&mp4, MediaFormat::Avc, None).expect("demux avc");
    assert_eq!(d.total, 2);
    let mut dec = TrackDecoder::new(MediaFormat::Avc);
    let mut got = 0usize;
    for g in &d.groups {
        let out = decode_group(&mut dec, &d, g).expect("decode avc group");
        got += out.len();
    }
    assert_eq!(got, 2, "baseline avc must decode 2/2");
    #[cfg(target_os = "macos")]
    assert!(
        !take_hw_fallback_hit(),
        "baseline avc decode on mac should stay HW (no fallback)"
    );
    #[cfg(not(target_os = "macos"))]
    let _ = take_hw_fallback_hit();

    // 回退标记语义本身：记一次即能取走并清零。
    assert!(!take_hw_fallback_hit());
    note_hw_fallback();
    assert!(take_hw_fallback_hit());
    assert!(!take_hw_fallback_hit());
}

/// High（含 B 帧）源 mac 上走硬解且按 PTS 排回显示序（mac-only）。
///
/// 料：固件侧现货 `src/diag/data/video-avc.mp4`（ffmpeg 制，H.264 High，
/// 160x120x10，含 1 B 帧自带 ctts；相对本文件 `include_bytes!`，同一仓库内
/// 已入库小样片跨 crate 复用，不另存二进制）。
/// 断言：解码序 PTS 必乱序（料有效门，无乱序说明料/ctts 坏了直接失败）；
/// 生产 `decode_group` 零回退（HW 真跑）、帧数 1:1、输出序与 CPU 显示序
/// 逐帧最佳匹配恒等（VT 像素与 rusty 逐字节一致，容差平均 SAD ≤ 1.0/像素
/// 防跨版本 IDCT 漂移）。
#[cfg(target_os = "macos")]
#[test]
fn avc_high_decodes_hw_with_pts_reorder() {
    let data: &[u8] = include_bytes!("../../../src/diag/data/video-avc.mp4");
    let d = demux_groups(data, MediaFormat::Avc, None).expect("demux avc");
    assert_eq!(d.total, 10);
    // ctts 乱序＝解码序≠显示序＝料有效（含 B 帧）；恒有序则料废。
    let pts_seq: Vec<i128> = d.groups.iter().flatten().map(|r| r.pts).collect();
    assert!(
        pts_seq.windows(2).any(|w| w[1] < w[0]),
        "fixture has no PTS disorder; check ctts in src/diag/data/video-avc.mp4"
    );

    // CPU 显示序参考（整流 Annex-B 全量解码）。
    let mut annexb = Vec::new();
    annexb.extend_from_slice(&d.annexb_prefix);
    for r in d.groups.iter().flatten() {
        annexb.extend(crate::codecs::length_prefixed_to_annexb(
            &r.payload,
            d.length_size,
        ));
    }
    let cpu = rusty_h264_decoder::Decoder::new()
        .decode_stream(&annexb)
        .expect("cpu decode");
    assert_eq!(cpu.len(), 10, "cpu must decode all 10");
    let cpu_y: Vec<Vec<u8>> = cpu
        .iter()
        .map(|f| {
            let p = f.as_planes();
            assert!(p.is_tight());
            p.y.to_vec()
        })
        .collect();

    // 生产 VT 路：零回退 + 帧数 1:1 + 逐帧最佳匹配恒等（保序）。
    let _ = take_hw_fallback_hit();
    let mut dec = TrackDecoder::new(MediaFormat::Avc);
    let mut vt_y: Vec<Vec<u8>> = Vec::new();
    for g in &d.groups {
        for f in decode_group(&mut dec, &d, g).expect("vt decode group") {
            vt_y.push(f.y);
        }
    }
    assert!(
        !take_hw_fallback_hit(),
        "high avc decode on mac should stay HW (no fallback)"
    );
    assert_eq!(vt_y.len(), 10, "vt must decode all 10 (no tail loss)");
    for (i, vy) in vt_y.iter().enumerate() {
        let mut best = (usize::MAX, u64::MAX);
        for (j, cy) in cpu_y.iter().enumerate() {
            let sad: u64 = vy
                .iter()
                .zip(cy.iter())
                .map(|(a, b)| (*a as i16 - *b as i16).abs() as u64)
                .sum();
            if sad < best.1 {
                best = (j, sad);
            }
        }
        let avg = best.1 as f64 / vy.len() as f64;
        assert_eq!(
            best.0, i,
            "vt[{i}] best matches cpu[{}] (order broken), avg SAD={avg:.3}",
            best.0
        );
        assert!(
            avg <= 1.0,
            "vt[{i}] diverges from cpu[{i}]: avg SAD={avg:.3}"
        );
    }
}

/// H.263 目标档位：源能装下的最大标准档（用户 1440p→16CIF 的依据）。
#[test]
fn h263_target_size_picks_largest_fitting() {
    use crate::codecs::h263_target_size;
    assert_eq!(h263_target_size(2560, 1440), (1408, 1152));
    assert_eq!(h263_target_size(1920, 1080), (704, 576)); // 16CIF 高 1152 超出 1080
    assert_eq!(h263_target_size(800, 600), (704, 576));
    assert_eq!(h263_target_size(400, 300), (352, 288));
    assert_eq!(h263_target_size(200, 150), (176, 144));
    assert_eq!(h263_target_size(100, 80), (128, 96));
    assert_eq!(h263_target_size(64, 48), (128, 96));
    assert_eq!(h263_target_size(0, 0), (128, 96));
}

/// H.263 黑边几何：16:9 源进 4CIF 精确尺寸、四角黑、中央承源。
#[test]
fn h263_letterbox_geometry_and_bars() {
    use crate::codecs::h263_letterbox;
    let f = solid_yuv(1920, 1080, 235);
    let out = h263_letterbox(&f);
    assert_eq!((out.w, out.h), (704, 576));
    assert_eq!(out.y.len(), 704 * 576);
    // 1080p 16:9 内接 4CIF → 704x396，上下各 90 黑边。
    assert_eq!(out.y[0], 0);
    assert_eq!(out.y[704 * 576 - 1], 0);
    assert_eq!(out.u[0], 128);
    assert_eq!(out.v[0], 128);
    // 中央行落在内接区（承源白）。
    assert_eq!(out.y[288 * 704 + 352], 235);
}

/// 用户 crash 精确复现：1920x1080 进 H.263 不再拒收（旧管线直缩 1080p 必炸）。
#[test]
fn h263_accepts_letterboxed_1080p() {
    use crate::codecs::{encode_h263, h263_letterbox};
    // 用户源几何：2560x1440 → 16CIF（最大档）。
    let out = h263_letterbox(&solid_yuv(2560, 1440, 128));
    assert_eq!((out.w, out.h), (1408, 1152));
    let bytes = encode_h263(std::slice::from_ref(&out)).expect("h263 encode 16CIF");
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..3], &[0, 0, 0x80], "want H.263 PSC start");
}

/// 1080p 对 mpeg2v/theora 编码器无尺寸门（同 bug 回归锁）。
#[test]
fn mpeg2v_theora_accept_1080p() {
    use crate::codecs::{encode_mpeg2v, encode_theora};
    let f = solid_yuv(1920, 1080, 128);
    let m2v = encode_mpeg2v(std::slice::from_ref(&f)).expect("mpeg2v 1080p");
    assert_eq!(&m2v[..4], &[0, 0, 1, 0xB3], "want MPEG sequence header");
    let t = encode_theora(std::slice::from_ref(&f)).expect("theora 1080p");
    assert_eq!(t.header_packets.len(), 3);
    assert!(!t.data_packets.is_empty());
}

/// 端到端：小 mp4 → H.263 整管线（缩放分支 + 裸流落盘，首包 PSC）。
#[test]
fn run_job_end_to_end_h263() {
    use crate::transcode::{run_transcode_job, TranscodeTarget};
    use std::sync::atomic::AtomicBool;
    let dir = std::env::temp_dir().join(format!("uefi-player-tc-h263-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("in.mp4");
    let out = dir.join("video.h263");
    // 源：2 帧 avc → mp4（与 `run_job_end_to_end_avc` 同式）。
    let st = encode_avc(&[solid_yuv(176, 144, 16), solid_yuv(176, 144, 235)]).expect("avc");
    let entry = avc_entry(&st.sps, &st.pps, 176, 144);
    let samples: Vec<Mp4OutSample> = st
        .samples
        .iter()
        .map(|x| Mp4OutSample {
            keyframe: x.keyframe,
            payload: length_prefixed_nals(&x.nalus),
        })
        .collect();
    let mp4 = mux_mp4_video(entry, &samples, 176, 144, 30).expect("mux");
    std::fs::write(&src, &mp4).expect("write src");
    let cancel = AtomicBool::new(false);
    let r = run_transcode_job(
        &src.display().to_string(),
        TranscodeTarget::H263,
        &out.display().to_string(),
        &cancel,
        |_| {},
        |_, _| {},
        None,
        None,
        10,
    );
    let info = r.expect("run_transcode_job h263");
    let produced = std::fs::read(&info.path).expect("read output");
    assert!(!produced.is_empty());
    assert_eq!(&produced[..3], &[0, 0, 0x80], "want H.263 PSC start");
    let _ = std::fs::remove_dir_all(&dir);
}
