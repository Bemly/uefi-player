//! 容器拆包（播放器 `Clip` + 壁纸共用）：MP4（石雨堂 `shiguredo_mp4`，
//! avc/hevc/vp9 三轨）+ OggS（theora 手写，`uefi-rs` 无现成拆包轮子）。
//!
//! 轮子结论（根 `AGENTS.md`「轮子优先」，2026-10-21 实测，见 `docs/player.md`）：
//! - MP4 拆包搜过 crates.io MP4 解复用：`shiguredo_mp4 2026.5.0`（本仓已有依赖，
//!   零第三方依赖、`no_std` 原生——`demux_mp4_file.rs` 仅文档注释含 `std::`，
//!   `categories = ["no-std"]`，`avcC/hvcC/vpcC` 全有）直接用，不自研。
//! - OggS 拆包：theora 箱只吃已拆帧 packet（Ogg framing 不在箱内），crates.io
//!   无 no_std Ogg 页拆包现货（只有整套 ogg 解码器，拖 std），故手写最小页拆帧
//!   （`OggS` 头 + 段表组包 + CRC32 校验，约 100 行，无新依赖）。
//! - MP4 样片形状 host 实测（`/tmp/mp4probe`，ffmpeg 9.0.1 制 160x120×10 帧）：
//!   avc 三路（shiguredo 拆 avcC + 转 Annex-B + `rusty_h264_decoder`）10 帧一致；
//!   hevc（`Hev1` + hvcc 4 数组 + 转 Annex-B + `rusty_h265`）10 帧；
//!   vp9（`Vp09` 样片即裸 VP9 帧，无长度前缀，直送 `vp9dec` 首帧即中）。
//!
//! 口径：坏样片/坏页计数跳过（调用方 `Clip` 同策略：全坏报错）；Ogg CRC 错页跳过。

use alloc::vec::Vec;

use shiguredo_mp4::TrackKind;
use shiguredo_mp4::boxes::SampleEntry;
use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};

/// MP4 目标视频轨（`[play]/[wallpaper] format` 选中谁拆谁）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4VideoKind {
    Avc,
    Hevc,
    Vp9,
}

/// MP4 拆出的视频轨：avc/hevc 走 Annex-B（参数集已拼在头部），
/// vp9 走裸帧（`Vp09` 样片即完整 VP9 帧，无长度前缀，直送解码器）。
/// 几何以解码器自报为准（容器头只进串口日志）。
pub struct Mp4Track {
    /// avc/hevc 用：`00 00 00 01` 分隔的 Annex-B 流（含头部参数集）。
    pub annexb: Vec<u8>,
    /// vp9 用：逐样片裸帧。
    pub raw_frames: Vec<Vec<u8>>,
}

fn annexb_push(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

fn fail(msg: &str) -> crate::error::NyaStatus {
    crate::error::NyaStatus::_Debug(alloc::string::String::from(msg))
}

/// MP4 整文件拆指定视频轨（`tag` 只进串口）。
/// 多轨文件只收首个匹配轨（音频/字幕轨直接忽略）；Hev1（in-band）与
/// Hvc1（out-of-band）同一路（参数集拼头部，in-band 重复无害，解码器照收）。
pub fn demux_mp4_track(
    data: &[u8],
    want: Mp4VideoKind,
    tag: &str,
) -> crate::error::Result<Mp4Track> {
    let mut dx = Mp4FileDemuxer::new();
    dx.handle_input(Input { position: 0, data });
    let mut annexb = Vec::new();
    let mut raw_frames: Vec<Vec<u8>> = Vec::new();
    let mut w = 0usize;
    let mut h = 0usize;
    let mut target: Option<u32> = None;
    let mut length_size = 4usize;
    let mut skipped = 0u32;
    let mut total = 0u32;
    // 首样片 entry 决定轨类型（entry 缺省时沿用同轨首样片结论）。
    let mut kinds: Vec<(u32, Mp4VideoKind)> = Vec::new();
    loop {
        let s = dx
            .next_sample()
            .map_err(|e| fail(&alloc::format!("{}: mp4 demux error: {:?}", tag, e)))?;
        let Some(s) = s else { break };
        if s.track.kind != TrackKind::Video {
            continue;
        }
        let kind = match s.sample_entry {
            Some(SampleEntry::Avc1(_)) => Some(Mp4VideoKind::Avc),
            Some(SampleEntry::Hev1(_)) | Some(SampleEntry::Hvc1(_)) => Some(Mp4VideoKind::Hevc),
            Some(SampleEntry::Vp09(_)) | Some(SampleEntry::Vp08(_)) => Some(Mp4VideoKind::Vp9),
            Some(_) => None,
            // entry 缺省：沿用该轨首样片结论（未知轨直接忽略）。
            None => kinds
                .iter()
                .find(|(id, _)| *id == s.track.track_id)
                .map(|(_, k)| *k),
        };
        let Some(kind) = kind else { continue };
        if !kinds.iter().any(|(id, _)| *id == s.track.track_id) {
            kinds.push((s.track.track_id, kind));
            // 首样片：取参 + 几何 + 建轨。
            if target.is_none() && kind == want {
                target = Some(s.track.track_id);
                match s.sample_entry {
                    Some(SampleEntry::Avc1(b)) => {
                        length_size = (b.avcc_box.length_size_minus_one.get() + 1) as usize;
                        for sps in &b.avcc_box.sps_list {
                            annexb_push(&mut annexb, sps);
                        }
                        for pps in &b.avcc_box.pps_list {
                            annexb_push(&mut annexb, pps);
                        }
                        w = b.visual.width as usize;
                        h = b.visual.height as usize;
                    }
                    Some(SampleEntry::Hev1(b)) => {
                        length_size = (b.hvcc_box.length_size_minus_one.get() + 1) as usize;
                        for arr in &b.hvcc_box.nalu_arrays {
                            for nalu in &arr.nalus {
                                annexb_push(&mut annexb, nalu);
                            }
                        }
                        w = b.visual.width as usize;
                        h = b.visual.height as usize;
                    }
                    Some(SampleEntry::Hvc1(b)) => {
                        length_size = (b.hvcc_box.length_size_minus_one.get() + 1) as usize;
                        for arr in &b.hvcc_box.nalu_arrays {
                            for nalu in &arr.nalus {
                                annexb_push(&mut annexb, nalu);
                            }
                        }
                        w = b.visual.width as usize;
                        h = b.visual.height as usize;
                    }
                    Some(SampleEntry::Vp09(b)) => {
                        w = b.visual.width as usize;
                        h = b.visual.height as usize;
                    }
                    Some(SampleEntry::Vp08(b)) => {
                        w = b.visual.width as usize;
                        h = b.visual.height as usize;
                    }
                    _ => {}
                }
            }
        }
        if Some(s.track.track_id) != target {
            continue;
        }
        total += 1;
        let end = (s.data_offset as usize).saturating_add(s.data_size);
        if end > data.len() || (s.data_offset as usize) > data.len() {
            skipped += 1;
            continue;
        }
        let bytes = &data[s.data_offset as usize..end];
        match want {
            Mp4VideoKind::Vp9 => {
                if bytes.is_empty() {
                    skipped += 1;
                } else {
                    raw_frames.push(bytes.to_vec());
                }
            }
            Mp4VideoKind::Avc | Mp4VideoKind::Hevc => {
                // 长度前缀 NALU 流 → Annex-B（坏长度即坏样片，记数跳过）。
                let mut off = 0usize;
                let mut ok = false;
                while off + length_size <= bytes.len() {
                    let mut n = 0usize;
                    for i in 0..length_size {
                        n = (n << 8) | bytes[off + i] as usize;
                    }
                    off += length_size;
                    if n == 0 || off + n > bytes.len() {
                        break;
                    }
                    annexb_push(&mut annexb, &bytes[off..off + n]);
                    off += n;
                    ok = true;
                }
                if !ok {
                    skipped += 1;
                }
            }
        }
    }
    if target.is_none() {
        return Err(fail(&alloc::format!(
            "{}: no {:?} video track in mp4",
            tag,
            want
        )));
    }
    log::info!(
        "{}: mp4 track container {}x{} ({} samples)",
        tag,
        w,
        h,
        total
    );
    if skipped > 0 {
        log::warn!("{}: mp4 demux skipped {} bad sample(s)", tag, skipped);
    }
    if total == 0 {
        return Err(fail(&alloc::format!("{}: mp4 track has no samples", tag)));
    }
    Ok(Mp4Track { annexb, raw_frames })
}

/// Ogg CRC32（MSB-first 非反射，多项式 `0x04C11DB7`，Ogg 规范口径；
/// 注意不是 zlib 反射版 `0xEDB88320`——host 实测反射版全页 mismatch，
/// MSB-first 逐位版与文件 CRC 一致）：编译期建表，零源膨胀
/// （`const fn` 循环，运行时查表）。
const fn crc_entry(i: u32) -> u32 {
    let mut c = i << 24;
    let mut k = 0;
    while k < 8 {
        c = if c & 0x8000_0000 != 0 {
            0x04C11DB7 ^ (c << 1)
        } else {
            c << 1
        };
        k += 1;
    }
    c
}

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = crc_entry(i as u32);
        i += 1;
    }
    t
}

const CRC_TABLE: [u32; 256] = crc_table();

/// OggS 页拆包（theora 用）：返回组好的 packet 序列（ident/comment/setup/data…
/// 按流内出现顺序）。多逻辑流文件只收首个序列号的流，其余忽略；
/// CRC 错页记数跳过（`tag` 串口留数，全坏调用方报错）。
pub fn demux_ogg_packets(data: &[u8], tag: &str) -> crate::error::Result<Vec<Vec<u8>>> {
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut serial: Option<u32> = None;
    let mut skipped = 0u32;
    let mut off = 0usize;
    while off + 27 <= data.len() {
        // 页头：`OggS` + 版本 0 + 类型 + granule(8) + 序列号(4) + 页序号(4)
        // + CRC(4) + 段数(1)。
        if &data[off..off + 4] != b"OggS" {
            return Err(fail(&alloc::format!(
                "{}: ogg sync lost at offset {}",
                tag,
                off
            )));
        }
        if data[off + 4] != 0 {
            return Err(fail(&alloc::format!(
                "{}: unsupported ogg version {}",
                tag,
                data[off + 4]
            )));
        }
        let pageno = u32::from_le_bytes([
            data[off + 18],
            data[off + 19],
            data[off + 20],
            data[off + 21],
        ]);
        let _ = pageno;
        let ser = u32::from_le_bytes([
            data[off + 14],
            data[off + 15],
            data[off + 16],
            data[off + 17],
        ]);
        let nsegs = data[off + 26] as usize;
        if off + 27 + nsegs > data.len() {
            break;
        }
        let segtab = &data[off + 27..off + 27 + nsegs];
        let body_len: usize = segtab.iter().map(|s| *s as usize).sum();
        let body_off = off + 27 + nsegs;
        if body_off + body_len > data.len() {
            break;
        }
        if serial.is_none() {
            serial = Some(ser);
        }
        // 非首流页：整页忽略（不组包，不断同步）。
        if Some(ser) != serial {
            off = body_off + body_len;
            continue;
        }
        // CRC 校验（校验域先置零再算）。
        let mut head = [0u8; 27];
        head.copy_from_slice(&data[off..off + 27]);
        head[22..26].copy_from_slice(&[0, 0, 0, 0]);
        let mut crc = 0u32;
        for b in head
            .iter()
            .chain(segtab.iter())
            .chain(data[body_off..body_off + body_len].iter())
        {
            crc = CRC_TABLE[(((crc >> 24) ^ (*b as u32)) & 0xFF) as usize] ^ crc.wrapping_shl(8);
        }
        let want_crc = u32::from_le_bytes([
            data[off + 22],
            data[off + 23],
            data[off + 24],
            data[off + 25],
        ]);
        if crc != want_crc {
            skipped += 1;
            off = body_off + body_len;
            continue;
        }
        // 按段表组 packet（255 = 续段）。
        let mut boff = body_off;
        for s in segtab {
            let len = *s as usize;
            cur.extend_from_slice(&data[boff..boff + len]);
            boff += len;
            if len < 255 {
                packets.push(core::mem::take(&mut cur));
            }
        }
        off = body_off + body_len;
    }
    if !cur.is_empty() {
        // 尾包无终结段（文件截断）：照收，解码器自会拒绝坏包。
        packets.push(cur);
    }
    if skipped > 0 {
        log::warn!("{}: ogg skipped {} bad page(s)", tag, skipped);
    }
    if packets.is_empty() {
        return Err(fail(&alloc::format!(
            "{}: no ogg packets ({} bad pages)",
            tag,
            skipped
        )));
    }
    Ok(packets)
}
