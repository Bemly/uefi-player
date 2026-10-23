//! 转码页：MP4/Ogg 封装（拆自 `transcode.rs`，行为零改）。

use super::*;

pub(crate) struct Mp4OutSample {
    pub(crate) keyframe: bool,
    pub(crate) payload: Vec<u8>,
}

/// NAL 串 → 长度前缀样片负载（MP4 样片标准形态）。
pub(crate) fn length_prefixed_nals(nalus: &[Vec<u8>]) -> Vec<u8> {
    let mut pl = Vec::new();
    for nalu in nalus {
        pl.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        pl.extend_from_slice(nalu);
    }
    pl
}

pub(crate) fn visual_fields(w: u32, h: u32) -> VisualSampleEntryFields {
    use shiguredo_mp4::boxes::VisualSampleEntryFields;
    VisualSampleEntryFields {
        data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
        width: w as u16,
        height: h as u16,
        horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
        vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
        frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
        compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
        depth: VisualSampleEntryFields::DEFAULT_DEPTH,
    }
}

/// AVC entry（`avc1/avcC`，profile/level 从 SPS 读）。
pub(crate) fn avc_entry(sps: &[u8], pps: &[u8], w: u32, h: u32) -> SampleEntry {
    use shiguredo_mp4::boxes::{Avc1Box, AvccBox};
    SampleEntry::Avc1(Avc1Box {
        visual: visual_fields(w, h),
        avcc_box: AvccBox {
            avc_profile_indication: sps.get(1).copied().unwrap_or(66),
            profile_compatibility: sps.get(2).copied().unwrap_or(0),
            avc_level_indication: sps.get(3).copied().unwrap_or(30),
            length_size_minus_one: Uint::new(3),
            sps_list: vec![sps.to_vec()],
            pps_list: vec![pps.to_vec()],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext_list: vec![],
        },
        unknown_boxes: vec![],
    })
}

/// MP4/IVF level 选档（avc/hevc/vp9 共用口径；L4.x 上限 2228224 像素，
/// L5.x 上限 8912896 像素——两家表同值；1440p 落第二档，4K 方图落第三档）：
/// 0 = L4.x（调用方原口径），1 = L5.0，2 = L5.1+/L6.0。
pub(crate) fn mp_level_tier(w: u32, h: u32) -> u32 {
    let px = w as u64 * h as u64;
    if px > 8_912_896 {
        2
    } else if px > 2_228_224 {
        1
    } else {
        0
    }
}

/// HEVC entry（`hvc1/hvcC`；固件 demux 不校验 profile 字段，
/// 此处按 Main 8-bit 420 填保守值；level 按分辨率取（L4.x/5.0/5.1），
/// 名义时钟 30fps（码控口径见 `codecs.rs` 头注释），1440p60+ 名义上要更高 tier，
/// 固件侧不校验、主流播放器按实际 SPS 容忍，此处不另细分）。
pub(crate) fn hevc_entry(
    vps: &[u8],
    sps: &[u8],
    pps: &[u8],
    w: u32,
    h: u32,
    fps: u32,
) -> SampleEntry {
    use shiguredo_mp4::boxes::{Hvc1Box, HvccBox, HvccNalUintArray};
    let array = |t: u8, nalus: Vec<Vec<u8>>| HvccNalUintArray {
        array_completeness: Uint::new(1),
        nal_unit_type: Uint::new(t),
        nalus,
    };
    let level_idc = match mp_level_tier(w, h) {
        2 => 153, // L5.1
        1 => 150, // L5.0（1440p 档）
        _ => {
            if fps > 30 {
                123
            } else {
                120
            }
        }
    };
    SampleEntry::Hvc1(Hvc1Box {
        visual: visual_fields(w, h),
        hvcc_box: HvccBox {
            general_profile_space: Uint::new(0),
            general_tier_flag: Uint::new(0),
            general_profile_idc: Uint::new(1),
            general_profile_compatibility_flags: 0x6000_0000,
            general_constraint_indicator_flags: Uint::new(0),
            general_level_idc: level_idc,
            min_spatial_segmentation_idc: Uint::new(0),
            parallelism_type: Uint::new(0),
            chroma_format_idc: Uint::new(1),
            bit_depth_luma_minus8: Uint::new(0),
            bit_depth_chroma_minus8: Uint::new(0),
            avg_frame_rate: (fps.saturating_mul(256)) as u16,
            constant_frame_rate: Uint::new(0),
            num_temporal_layers: Uint::new(1),
            temporal_id_nested: Uint::new(1),
            length_size_minus_one: Uint::new(3),
            nalu_arrays: vec![
                array(32, vec![vps.to_vec()]),
                array(33, vec![sps.to_vec()]),
                array(34, vec![pps.to_vec()]),
            ],
        },
        unknown_boxes: vec![],
    })
}

/// VP9 entry（`vp09/vpcC`；裸帧样片，固件直送解码器；level 按分辨率取，
/// 与 hevc 同表：L4.1/5.0/6.0）。
pub(crate) fn vp9_entry(w: u32, h: u32) -> SampleEntry {
    use shiguredo_mp4::boxes::{Vp09Box, VpccBox};
    let level = match mp_level_tier(w, h) {
        2 => 60, // L6.0
        1 => 50, // L5.0（1440p 档）
        _ => 41, // L4.1
    };
    SampleEntry::Vp09(Vp09Box {
        visual: visual_fields(w, h),
        vpcc_box: VpccBox {
            profile: 0,
            level,
            bit_depth: Uint::new(8),
            chroma_subsampling: Uint::new(1),
            video_full_range_flag: Uint::new(1),
            colour_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            codec_initialization_data: Vec::new(),
        },
        unknown_boxes: vec![],
    })
}

/// MP4 打包通用路（`Mp4FileMuxer`；entry 决定 avc1/hvc1/vp09）。
pub(crate) fn mux_mp4_video(
    entry: SampleEntry,
    samples: &[Mp4OutSample],
    w: u32,
    h: u32,
    fps: u32,
) -> Result<Vec<u8>, String> {
    use shiguredo_mp4::mux::{Mp4FileMuxer, Sample};
    use shiguredo_mp4::TrackKind;
    use std::num::NonZeroU32;
    let _ = (w, h);
    if samples.is_empty() {
        return Err(String::from("mp4: no samples"));
    }
    let mut muxer = Mp4FileMuxer::new().map_err(|e| format!("mux init: {e:?}"))?;
    let timescale = NonZeroU32::new(90000).unwrap();
    let dur = 90000u32 / fps.max(1);
    let mut data_offset = muxer.initial_boxes_bytes().len() as u64;
    for (i, s) in samples.iter().enumerate() {
        muxer
            .append_sample(&Sample {
                track_kind: TrackKind::Video,
                sample_entry: if i == 0 { Some(entry.clone()) } else { None },
                keyframe: s.keyframe,
                timescale,
                duration: dur,
                composition_time_offset: None,
                data_offset,
                data_size: s.payload.len(),
            })
            .map_err(|e| format!("mux append: {e:?}"))?;
        data_offset += s.payload.len() as u64;
    }
    let initial = muxer.initial_boxes_bytes().to_vec();
    let finalized = muxer
        .finalize()
        .map_err(|e| format!("mux finalize: {e:?}"))?;
    // 组装：head（pairs 改写）+ mdat 头（initial 尾部 16B，pairs 改写终值）+
    // 样片负载（紧跟 initial）+ moov（pairs 偏移）。
    let mut max_end = initial.len() + samples.iter().map(|s| s.payload.len()).sum::<usize>();
    for (off, b) in finalized.offset_and_bytes_pairs() {
        max_end = max_end.max(off as usize + b.len());
    }
    let mut file = vec![0u8; max_end];
    file[..initial.len()].copy_from_slice(&initial);
    for (off, b) in finalized.offset_and_bytes_pairs() {
        let o = off as usize;
        file[o..o + b.len()].copy_from_slice(b);
    }
    let mut pos = initial.len();
    for s in samples {
        file[pos..pos + s.payload.len()].copy_from_slice(&s.payload);
        pos += s.payload.len();
    }
    Ok(file)
}

/// Theora → OggS 打包（三头 + 数据包同一逻辑流；CRC 由 `ogg` 轮子算，
/// 与固件手写拆包同算法，roundtrip 测试锁死）。
pub(crate) fn mux_ogg_theora(headers: &[Vec<u8>], packets: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    use ogg::{PacketWriteEndInfo, PacketWriter};
    if headers.len() != 3 || packets.is_empty() {
        return Err(String::from("ogg: need 3 headers + data"));
    }
    let mut out = Vec::new();
    let mut w = PacketWriter::new(&mut out);
    let serial: u32 = 0x1951_0715;
    for (i, h) in headers.iter().enumerate() {
        w.write_packet(
            h.clone(),
            serial,
            PacketWriteEndInfo::NormalPacket,
            i as u64,
        )
        .map_err(|e| format!("ogg header: {e}"))?;
    }
    for (i, p) in packets.iter().enumerate() {
        let last = i + 1 == packets.len();
        w.write_packet(
            p.clone(),
            serial,
            if last {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            },
            // theora granule：帧序号单调递增（固件拆包不消费，仅保单调）。
            (headers.len() + i) as u64,
        )
        .map_err(|e| format!("ogg packet: {e}"))?;
    }
    drop(w);
    Ok(out)
}
