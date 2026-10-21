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

/// Annex-B 接入单元切分（流式解码用：调用方逐 AU 喂解码器，替代整流投喂）。
/// `data` 为起始码分隔的 Annex-B 流（`00 00 01` / `00 00 00 01`），
/// `vcl` 为 VCL NAL 类型集（avc 按 1 字节头取值 `1..=5`，
/// hevc 按 2 字节头 `(b[0] >> 1) & 0x3f` 取值），`nal_hdr_len` 为 NAL 头长
/// （avc=1，hevc=2）。调用方按首个 VCL NAL 区分编码选表，
/// 本函数保持编码无关。返回 AU 的字节区间（借用切片零拷贝）。
///
/// 规则（单 slice 流假设，ffmpeg 默认即单 slice，样片见 `src/diag/data/`）：
/// - 按起始码切 NALU（4 字节优先，`00 00 00 01` 不误判成 3 字节码；
///   合法负载内 `00 00 01` 由编码器 emulation（`00 00 03`）保证不出现，
///   故裸扫安全，坏流按下条截断）。
/// - AU 边界 = 每个 VCL NAL 起一组；参数集/SEI/AUD 等非 VCL NAL 归入其后
///   首个 AU（前导字节一并归首组，首组起点恒 0；无 VCL 的尾段并入末组；
///   整流无 VCL 则整流一组，解码器自会报 0 帧）。
/// - 返回 Range 彼此相邻（无重叠无空洞），覆盖 `[0, 截断点)`；
///   全程越界守卫，首个头不完整 NAL 处直接截断（`log::warn` 记数，不 panic）。
///
/// 多 slice 流退化：同帧多 slice 会被拆成多个 AU（形状仍连续合法），
/// 逐 AU 独立解码可能出重复/半帧——本函数只保切分形状（见单测口径注释），
/// 逐帧等价由 QEMU 侧逐 AU 解码拼接比对。
pub fn split_annexb_aus(
    data: &[u8],
    vcl: &[u8],
    nal_hdr_len: usize,
) -> Vec<core::ops::Range<usize>> {
    let mut aus: Vec<core::ops::Range<usize>> = Vec::new();
    if data.is_empty() {
        return aus;
    }
    if nal_hdr_len == 0 {
        // 调用方传参错误：无头可取，无从分类，直接空返回（不猜）。
        log::warn!("au-split: nal_hdr_len == 0, drop {} byte(s)", data.len());
        return aus;
    }
    // 1) 起始码扫描：逐个记 NALU（起始码偏移，负载偏移）。
    let mut nalus: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                nalus.push((i, i + 3));
                i += 3;
                continue;
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                nalus.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    if nalus.is_empty() {
        return aus; // 无起始码：无 NALU，无 AU。
    }
    // 2) 逐 NALU 分类成组（坏头即截断， tiling 止于该 NALU 起点）。
    // 非 VCL 从不自己开组：挂起（`pend` 记首个），归入其后首个 AU；
    // VCL 到达时，上组（如已开）止于 `pend`（无挂起则止于本 VCL），
    // 新组起于同一点——上一 VCL 之后的非 VCL（含 AUD）全部归入新组。
    let mut au_start: Option<usize> = None;
    let mut pend: Option<usize> = None;
    let mut end = data.len();
    let mut cut = false;
    for (idx, &(start, payload)) in nalus.iter().enumerate() {
        let limit = if idx + 1 < nalus.len() {
            nalus[idx + 1].0
        } else {
            data.len()
        };
        if payload >= limit {
            continue; // 空 NALU（相邻起始码）：吸收，不建组不断流。
        }
        if payload + nal_hdr_len > limit {
            // 头不完整（流截断/坏长度）：截断， tiling 止于本 NALU 起点。
            end = start;
            cut = true;
            break;
        }
        let t = if nal_hdr_len == 1 {
            data[payload] & 0x1F
        } else {
            (data[payload] >> 1) & 0x3F
        };
        if vcl.contains(&t) {
            match au_start {
                Some(s) => {
                    let at = pend.unwrap_or(start);
                    aus.push(s..at);
                    au_start = Some(at);
                }
                // 首个 VCL：此前挂起的前导非 VCL 并入本 AU。
                None => au_start = pend.or(Some(start)),
            }
            pend = None;
        } else if pend.is_none() {
            pend = Some(start); // 非 VCL：挂起，归入其后首个 AU。
        }
    }
    match au_start {
        Some(s) if s < end => aus.push(s..end),
        // 整流无 VCL：挂起的前导即整流，整流一组（解码器自会报 0 帧）。
        None => {
            if let Some(p) = pend
                && p < end
            {
                aus.push(p..end);
            }
        }
        _ => {}
    }
    if cut {
        log::warn!("au-split: truncated annexb at {end} ({} au(s))", aus.len());
    }
    // 前导字节归首组（首组起点恒 0，保证无空洞）。
    if let Some(first) = aus.first_mut() {
        first.start = 0;
    }
    aus
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ops::Range;

    /// 连续无重叠无空洞断言（形状测试的公共口径）。
    fn assert_tiling(data: &[u8], aus: &[Range<usize>]) {
        assert!(!aus.is_empty(), "tiling must not be empty");
        assert_eq!(aus[0].start, 0, "first au must start at 0");
        for w in aus.windows(2) {
            assert_eq!(w[0].end, w[1].start, "aus must tile without gap/overlap");
        }
        assert_eq!(
            aus.last().unwrap().end,
            data.len(),
            "last au must reach end"
        );
        for a in aus {
            assert!(a.start < a.end, "au must be non-empty");
        }
    }

    /// 拼一个 AVC NALU（1 字节头，`four` 切 4 字节起始码否则 3 字节）。
    fn nal_avc(out: &mut Vec<u8>, four: bool, hdr: u8, body: &[u8]) {
        if four {
            out.extend_from_slice(&[0, 0, 0, 1]);
        } else {
            out.extend_from_slice(&[0, 0, 1]);
        }
        out.push(hdr);
        out.extend_from_slice(body);
    }

    /// 拼一个 HEVC NALU（2 字节头，首字节 `(type << 1)`，次字节固定 `0x01`）。
    fn nal_hevc(out: &mut Vec<u8>, four: bool, nal_type: u8, body: &[u8]) {
        if four {
            out.extend_from_slice(&[0, 0, 0, 1]);
        } else {
            out.extend_from_slice(&[0, 0, 1]);
        }
        out.push(nal_type << 1);
        out.push(0x01);
        out.extend_from_slice(body);
    }

    #[test]
    fn three_au_model_avc() {
        // 手造三 AU 模型流：SPS+PPS+IDR / SPS+PPS+P / SEI+IDR
        // （3/4 字节起始码混用，头用真值 0x67/0x68/0x65/0x41/0x06 走掩码）。
        let vcl: &[u8] = &[1, 2, 3, 4, 5];
        let mut s = Vec::new();
        nal_avc(&mut s, true, 0x67, &[1, 2]); // SPS
        nal_avc(&mut s, false, 0x68, &[3]); // PPS
        nal_avc(&mut s, false, 0x65, &[4, 5, 6]); // IDR → AU1
        nal_avc(&mut s, true, 0x67, &[7]); // SPS
        nal_avc(&mut s, true, 0x68, &[8]); // PPS
        nal_avc(&mut s, false, 0x41, &[9]); // P → AU2
        nal_avc(&mut s, false, 0x06, &[10]); // SEI（归入其后 AU3）
        nal_avc(&mut s, true, 0x65, &[11, 12]); // IDR → AU3
        let aus = split_annexb_aus(&s, vcl, 1);
        assert_eq!(aus.len(), 3, "three vcl nals -> three aus");
        assert_tiling(&s, &aus);
        // AU1 首 NAL 是挂起的 SPS（前导归首组），AU3 首 NAL 是 SEI。
        assert_eq!(&s[aus[0].start..aus[0].start + 5], &[0, 0, 0, 1, 0x67]);
        assert_eq!(&s[aus[2].start..aus[2].start + 4], &[0, 0, 1, 0x06]);
        // 每组恰含 1 个 VCL（组内重扫计数，不复用实现扫描器）。
        let mut vcl_total = 0;
        for a in &aus {
            let mut n = 0;
            let mut k = a.start;
            while k + 4 <= a.end {
                if s[k] == 0 && s[k + 1] == 0 && s[k + 2] == 1 {
                    if vcl.contains(&(s[k + 3] & 0x1F)) {
                        n += 1;
                    }
                    k += 3;
                } else if k + 5 <= a.end
                    && s[k] == 0
                    && s[k + 1] == 0
                    && s[k + 2] == 0
                    && s[k + 3] == 1
                {
                    if vcl.contains(&(s[k + 4] & 0x1F)) {
                        n += 1;
                    }
                    k += 4;
                } else {
                    k += 1;
                }
            }
            assert_eq!(n, 1, "each au holds exactly one vcl nal");
            vcl_total += n;
        }
        assert_eq!(vcl_total, 3);
    }

    #[test]
    fn two_au_model_hevc() {
        // VPS+SPS+PPS+IDR / AUD+TRAIL（2 字节头走 `(b >> 1) & 0x3F`）。
        let vcl: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17, 18, 19, 20, 21];
        let mut s = Vec::new();
        nal_hevc(&mut s, true, 32, &[1]); // VPS
        nal_hevc(&mut s, true, 33, &[2, 3]); // SPS
        nal_hevc(&mut s, false, 34, &[4]); // PPS
        nal_hevc(&mut s, false, 19, &[5, 6]); // IDR_W_RADL → AU1
        nal_hevc(&mut s, true, 35, &[7]); // AUD（归入其后 AU2）
        nal_hevc(&mut s, false, 1, &[8]); // TRAIL_R → AU2
        let aus = split_annexb_aus(&s, vcl, 2);
        assert_eq!(aus.len(), 2, "two vcl nals -> two aus");
        assert_tiling(&s, &aus);
        // AU2 起于 AUD（0x46 = 35 << 1）。
        assert_eq!(&s[aus[1].start..aus[1].start + 5], &[0, 0, 0, 1, 0x46]);
    }

    #[test]
    fn degenerate_shapes() {
        let vcl: &[u8] = &[1, 2, 3, 4, 5];
        // 空输入 / 无起始码：无 NALU，无 AU。
        assert!(split_annexb_aus(&[], vcl, 1).is_empty());
        assert!(split_annexb_aus(&[0xFF, 0x00, 0x01], vcl, 1).is_empty());
        // 非法头长：记数告警后空返回（不猜）。
        let mut s = Vec::new();
        nal_avc(&mut s, true, 0x65, &[1]);
        assert!(split_annexb_aus(&s, vcl, 0).is_empty());
        // 整流无 VCL：整流一组（解码器自会报 0 帧）。
        let mut s = Vec::new();
        nal_avc(&mut s, true, 0x67, &[1]);
        nal_avc(&mut s, false, 0x68, &[2]);
        let aus = split_annexb_aus(&s, vcl, 1);
        assert_eq!(aus, vec![0..s.len()]);
        // 前导杂字节归首组（首组起点恒 0）。
        let mut s = vec![0xFF, 0xFF];
        nal_avc(&mut s, true, 0x67, &[1]);
        nal_avc(&mut s, false, 0x65, &[2]);
        let aus = split_annexb_aus(&s, vcl, 1);
        assert_eq!(aus.len(), 1);
        assert_eq!(aus[0], 0..s.len());
        // 尾部头不完整：截断（tiling 止于坏 NAL 起点，形状仍连续）。
        let mut s = Vec::new();
        nal_hevc(&mut s, true, 33, &[1]);
        nal_hevc(&mut s, false, 19, &[2]);
        let cut = s.len();
        s.extend_from_slice(&[0, 0, 1, 0x26]); // 差 1 头字节：坏结构。
        let hevc_vcl: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17, 18, 19, 20, 21];
        let aus = split_annexb_aus(&s, hevc_vcl, 2);
        assert_eq!(aus.len(), 1);
        assert_eq!(aus[0], 0..cut);
    }

    #[test]
    fn real_mp4_samples_shape() {
        // 口径：`src/diag/data/video-avc.mp4` / `video-hevc.mp4` 均为
        // 160x120×10 帧（见 `docs/handover.md` §7.8，ffmpeg 默认单 slice →
        // 1 帧恰 1 VCL NAL → 10 AU）。本单测只保切分形状（组数 + tiling）；
        // 真正逐帧比对（逐 AU 独立解码拼接 vs 整流解码逐字节）在固件/QEMU 侧做
        // （Agent-A），此处不断言像素。
        let avc = include_bytes!("../diag/data/video-avc.mp4");
        let track = demux_mp4_track(avc, Mp4VideoKind::Avc, "au-shape").expect("avc mp4 demux");
        let aus = split_annexb_aus(&track.annexb, &[1, 2, 3, 4, 5], 1);
        assert_eq!(aus.len(), 10, "avc sample holds 10 frames");
        assert_tiling(&track.annexb, &aus);

        let hevc = include_bytes!("../diag/data/video-hevc.mp4");
        let track = demux_mp4_track(hevc, Mp4VideoKind::Hevc, "au-shape").expect("hevc mp4 demux");
        let hevc_vcl: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17, 18, 19, 20, 21];
        let aus = split_annexb_aus(&track.annexb, hevc_vcl, 2);
        assert_eq!(aus.len(), 10, "hevc sample holds 10 frames");
        assert_tiling(&track.annexb, &aus);
    }
}
