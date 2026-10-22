//! 全量预解码（`[play] feed = "clip"` 的落点，也是壁纸/mp 强制路径）：
//! 与拆分前 `decoder.rs` 逐行一致（默认零行为变化）；懒供片见 `super::lazy`。
//!
//! 解码并行（统一控制面）：BSP 全量解码，AP 永不解码/永不碰 GOP；SIMD 单比特
//! 跟随全局（本层不设门）。`qois`/`raw` 无状态帧级并行可行：`threads=all` 即
//! 兑现（分片顺序解码，AP 钩子预留，同字节输出）；有状态六格式 `threads=all`
//! 解析但 warn 仍顺序（状态机串行，交叠 deferred）。零拷：`raw` reinterpret
//! 零拷；其余按帧分配（全量语义，稳态复用在懒 `single`/窗侧）。

use crate::config::MediaFormat;
use alloc::vec;
use alloc::vec::Vec;
use uefi::proto::console::gop::BltPixel;

type EagerClip = super::Clip;
/// qois 字节流解码核心（`tag` 只进串口：`video` / `wallpaper`）。
/// 坏帧记数跳过，全坏/几何不一致直接报错（曾无限 rewind 空转）。
pub(crate) fn from_qois_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Qois);
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut offset = 0;
    let mut expect: Option<(u32, u32)> = None;
    let mut w = 0usize;
    let mut h = 0usize;

    while offset + 4 <= data.len() {
        let len_bytes = &data[offset..offset + 4];
        let frame_len =
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;

        let data_start = offset + 4;
        let next_frame_pos = data_start + frame_len;

        if next_frame_pos > data.len() {
            break;
        }

        let frame_data = &data[data_start..next_frame_pos];

        let (header, pixel_count) = match qoi::decode_header(frame_data) {
            Ok(h) => {
                let n = (h.width * h.height) as usize;
                if n == 0 || n > 16 * 1024 * 1024 {
                    skipped += 1;
                    offset = next_frame_pos;
                    continue;
                }
                (h, n)
            }
            // 真机错位多半落在这里：记数跳过（曾无限静默丢帧）。
            Err(_) => {
                skipped += 1;
                offset = next_frame_pos;
                continue;
            }
        };
        // 同一文件内几何必须一致（混入异形帧会撕裂后面所有帧定位）。
        match expect {
            None => {
                expect = Some((header.width, header.height));
                w = header.width as usize;
                h = header.height as usize;
            }
            Some((ew, eh)) if (header.width, header.height) != (ew, eh) => {
                skipped += 1;
                offset = next_frame_pos;
                continue;
            }
            _ => {}
        }

        let mut pixel_buffer = vec![BltPixel::new(0, 0, 0); pixel_count];

        if qoi::decode_to_buf(
            unsafe {
                core::slice::from_raw_parts_mut(
                    pixel_buffer.as_mut_ptr() as *mut u8,
                    pixel_count * 4,
                )
            },
            frame_data,
        )
        .is_err()
        {
            skipped += 1;
            offset = next_frame_pos;
            continue;
        };

        for pixel_buffer in pixel_buffer.iter_mut() {
            let r = pixel_buffer.red;
            pixel_buffer.red = pixel_buffer.blue;
            pixel_buffer.blue = r;
        }

        frames.push(pixel_buffer);
        offset = next_frame_pos;
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// hevc 字节流解码核心（`tag` 只进串口）：Annex-B 经 vendor `rusty_h265`
/// 整流解码（与 `diag::hevc_probe` 同路：`push_annexb` + `flush` + 取帧），
/// 坏帧计数跳过，全坏/几何不一致直接报错（与 `from_qois_bytes` 同策略）。
/// 片源：`1080p/video.hevc`（样片见 `src/diag/data/hevc_probe_main.265`）。
pub(crate) fn from_hevc_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Hevc);
    let mut dec = rusty_h265::Decoder::new();
    dec.push_annexb(data, None).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: hevc push error: {:?}", tag, e))
    })?;
    dec.flush();
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(usize, usize)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    // YUV 暂存跨帧复用（8 格式调研 P2：`write_yuv` 每帧 `Vec` 分配是加载期主分配项；
    // `w/h` 首帧冻结后 `ybytes + 2*uvbytes` 恒定，`clear` 保容量稳态零分配）。
    let mut yuv_scratch: Vec<u8> = Vec::new();
    loop {
        let f = match dec.next_frame() {
            Ok(f) => f,
            Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        // 同一文件内几何必须一致（与 qois 路同策略：混入异形帧跳过）。
        // 只收 8-bit（`write_yuv` 高位深按 2 字节/样点写，与本转换器不兼容）。
        if f.bit_depth() != 8 {
            skipped += 1;
            continue;
        }
        match expect {
            None => {
                expect = Some((f.width, f.height));
                w = f.width;
                h = f.height;
            }
            Some((ew, eh)) if (f.width, f.height) != (ew, eh) => {
                skipped += 1;
                continue;
            }
            _ => {}
        }
        // `yuv` 跨帧复用（`w/h` 首帧冻结后长度恒定，`clear` 保容量，稳态零分配；
        // 首帧前 `w/h == 0` 只走一次短路，见下 `ybytes` 门）。外层 `yuv_scratch`
        // 由循环外声明（8 格式调研 P2）。
        yuv_scratch.clear();
        f.write_yuv(&mut yuv_scratch);
        let yuv = &yuv_scratch;
        let ybytes = w.saturating_mul(h);
        let uvbytes = w.div_ceil(2).saturating_mul(h.div_ceil(2));
        // YUV420：Y + U + V（两色度平面与 Y 同半尺寸；短写即坏帧）。
        if yuv.len() < ybytes.saturating_add(uvbytes.saturating_mul(2)) {
            skipped += 1;
            continue;
        }
        let (ypl, rest) = yuv.split_at(ybytes);
        let (upl, vpl) = rest.split_at(uvbytes.min(rest.len()));
        if vpl.len() < uvbytes {
            skipped += 1;
            continue;
        }
        frames.push(super::Clip::yuv420_to_bgra(ypl, upl, vpl, w, h));
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// vp9 单样片解码结果收集（IVF/MP4 双路共用）：只要显示帧
/// （隐藏 altref 无画面，直接跳过），只收 8-bit 4:2:0。
/// 坏帧计数跳过，全坏/几何不一致调用方报错（与其它路同策略）。
fn collect_vp9_frame(
    frames: &mut Vec<Vec<BltPixel>>,
    skipped: &mut u32,
    expect: &mut Option<(u32, u32)>,
    w: &mut usize,
    h: &mut usize,
    decoded: Vec<vp9dec::DecodedFrame>,
) {
    for d in decoded {
        // 隐藏帧（altref）无显示画面：不收，不记坏。
        let Some(f) = d.frame else { continue };
        // 只收 8-bit 4:2:0（轮子输出的唯一固件口径；其余记数跳过）。
        if f.bit_depth != 8 || f.subsampling_x != 1 || f.subsampling_y != 1 {
            *skipped += 1;
            continue;
        }
        let (vp9dec::PlaneData::U8(y), vp9dec::PlaneData::U8(u), vp9dec::PlaneData::U8(v)) =
            (&f.y, &f.u, &f.v)
        else {
            *skipped += 1;
            continue;
        };
        match *expect {
            None => {
                *expect = Some((f.width, f.height));
                *w = f.width as usize;
                *h = f.height as usize;
            }
            Some((ew, eh)) if (f.width, f.height) != (ew, eh) => {
                *skipped += 1;
                continue;
            }
            _ => {}
        }
        frames.push(super::Clip::yuv420_to_bgra(y, u, v, *w, *h));
    }
}

/// vp9 字节流解码核心（`tag` 只进串口）：IVF 容器经 vendor `vp9dec`
/// 的 `IvfReader` 拆帧 + `Decoder::decode_frame` 逐帧解码，
/// 只要显示帧（`show_frame == 1`；隐藏 altref 无画面，直接跳过），
/// 8-bit 4:2:0 才收（其余记数跳过）。坏帧计数跳过，全坏/几何不一致
/// 直接报错（与 `from_qois_bytes` 同策略）。
/// 片源：`1080p/video.ivf`（样片见 `src/diag/data/vp9_probe.ivf`，
/// 160x120×10 帧，host 已验 `shown=10`）。
pub(crate) fn from_vp9_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Vp9);
    let mut rdr = vp9dec::ivf::IvfReader::new(data).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: vp9 ivf error: {:?}", tag, e))
    })?;
    let mut dec = vp9dec::Decoder::new();
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(u32, u32)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    for chunk in &mut rdr {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let decoded = match dec.decode_frame(chunk.data) {
            Ok(d) => d,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        collect_vp9_frame(
            &mut frames,
            &mut skipped,
            &mut expect,
            &mut w,
            &mut h,
            decoded,
        );
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// vp9 裸帧序列解码核心（MP4 `Vp09` 轨用）：样片即完整 VP9 帧，
/// 收集器与 IVF 路同一套（显示帧/8-bit 4:2:0 口径一致）。
pub(crate) fn from_vp9_raw_frames(
    samples: &[Vec<u8>],
    tag: &str,
) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Vp9);
    let mut dec = vp9dec::Decoder::new();
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(u32, u32)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    for s in samples {
        let decoded = match dec.decode_frame(s) {
            Ok(d) => d,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        collect_vp9_frame(
            &mut frames,
            &mut skipped,
            &mut expect,
            &mut w,
            &mut h,
            decoded,
        );
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// avc 字节流解码核心（`tag` 只进串口）：Annex-B 经 vendor
/// `rusty_h264_decoder` 整流解码（与 `diag::avc_probe` 同路），
/// YUV420 → BGRA（只收 tight 8-bit 4:2:0，坏帧计数跳过）。
/// 片源：MP4 `avcC` 参数集 + 样片转 Annex-B（见 `demux_mp4_track`）。
pub(crate) fn from_avc_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Avc);
    let decoded = rusty_h264_decoder::Decoder::new()
        .decode_stream(data)
        .map_err(|e| {
            crate::error::NyaStatus::_Debug(alloc::format!("{}: avc decode error: {:?}", tag, e))
        })?;
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(usize, usize)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    for f in &decoded {
        // 只收 tight 8-bit 4:2:0（串行解码器本就产出此形状；其余记数跳过）。
        let planes = f.as_planes();
        if !planes.is_tight() {
            skipped += 1;
            continue;
        }
        match expect {
            None => {
                expect = Some((f.width, f.height));
                w = f.width;
                h = f.height;
            }
            Some((ew, eh)) if (f.width, f.height) != (ew, eh) => {
                skipped += 1;
                continue;
            }
            _ => {}
        }
        frames.push(super::Clip::yuv420_to_bgra(
            planes.y, planes.u, planes.v, w, h,
        ));
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// h263 裸流解码核心（`tag` 只进串口）：vendor `h263-rs` 流式解码
/// （`H263State` + `H263Reader`，标准基线空选项；事务回滚语义下
/// 连续坏包直接截断，避免空转）。
/// 片源：`1080p/video.h263`（裸 H.263 基线流，无容器）。
pub(crate) fn from_h263_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::H263);
    use h263_rs::parser::H263Reader;
    let mut state = h263_rs::H263State::new(h263_rs::DecoderOption::empty());
    let mut reader = H263Reader::from_source(data);
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(u16, u16)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    loop {
        match state.decode_next_picture(&mut reader) {
            Ok(()) => {}
            Err(e) if e.is_eof_error() => break,
            Err(_) => {
                // 事务回滚后同字节重试必重错：记数并截断（坏流不断解析）。
                skipped += 1;
                break;
            }
        }
        let Some(pic) = state.get_last_picture() else {
            skipped += 1;
            continue;
        };
        let dims = match pic.format().into_width_and_height() {
            Some(d) => d,
            None => {
                skipped += 1;
                continue;
            }
        };
        match expect {
            None => {
                expect = Some(dims);
                w = dims.0 as usize;
                h = dims.1 as usize;
            }
            Some(ew) if ew != dims => {
                skipped += 1;
                continue;
            }
            _ => {}
        }
        let (y, u, v) = pic.as_yuv();
        // 4:2:0 紧凑平面校验（半帧不上屏）。
        if y.len() != w.saturating_mul(h)
            || u.len() != (w / 2).saturating_mul(h / 2)
            || v.len() != (w / 2).saturating_mul(h / 2)
        {
            skipped += 1;
            continue;
        }
        frames.push(super::Clip::yuv420_to_bgra(y, u, v, w, h));
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// mpeg2 裸流解码核心（`tag` 只进串口）：vendor `filmcraft-mpeg2v`
/// 按接入单元切分（`access_units` 找起始码）逐 AU 解码 + `flush` 收尾帧，
/// 只收 4:2:0（422/444 记数跳过）。pts 按 AU 序号递增。
/// 片源：`1080p/video.m2v`（裸基本流，无容器）。
pub(crate) fn from_mpeg2v_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Mpeg2v);
    let mut dec = filmcraft_mpeg2v::Decoder::new();
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut expect: Option<(u32, u32)> = None;
    let mut w = 0usize;
    let mut h = 0usize;
    let push_pic = |pic: &filmcraft_mpeg2v::Picture,
                    frames: &mut Vec<Vec<BltPixel>>,
                    skipped: &mut u32,
                    expect: &mut Option<(u32, u32)>,
                    w: &mut usize,
                    h: &mut usize| {
        // 只收 4:2:0（转换器唯一口径；422/444 记数跳过）。
        if pic.chroma != filmcraft_mpeg2v::ChromaFormat::Yuv420 {
            *skipped += 1;
            return;
        }
        match *expect {
            None => {
                *expect = Some((pic.width, pic.height));
                *w = pic.width as usize;
                *h = pic.height as usize;
            }
            Some((ew, eh)) if (pic.width, pic.height) != (ew, eh) => {
                *skipped += 1;
                return;
            }
            _ => {}
        }
        // 平面长度校验（半帧不上屏）。
        let yb = (*w).saturating_mul(*h);
        let cb = (*w / 2).saturating_mul(*h / 2);
        if pic.y.len() < yb || pic.cb.len() < cb || pic.cr.len() < cb {
            *skipped += 1;
            return;
        }
        frames.push(super::Clip::yuv420_to_bgra(
            &pic.y[..yb],
            &pic.cb[..cb],
            &pic.cr[..cb],
            *w,
            *h,
        ));
    };
    for (i, r) in filmcraft_mpeg2v::access_units(data).iter().enumerate() {
        match dec.decode(&data[r.clone()], i as i64) {
            Ok(pics) => {
                for pic in &pics {
                    push_pic(pic, &mut frames, &mut skipped, &mut expect, &mut w, &mut h);
                }
            }
            Err(_) => {
                skipped += 1;
            }
        }
    }
    for pic in dec.flush() {
        push_pic(&pic, &mut frames, &mut skipped, &mut expect, &mut w, &mut h);
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// theora 包序列解码核心（`tag` 只进串口）：vendor `oxideav-theora`
///（ident/comment/setup 三头 + 数据包），`crop_for_display` 取显示区，
/// 左下原点翻转后 YUV420 → BGRA。只收 4:2:0（422/444 明确跳过）。
/// 包源：`demux_ogg_packets`（OggS 是 theora 磁盘原格式，见 `demux.rs`）。
pub(crate) fn from_theora_packets(
    packets: &[Vec<u8>],
    tag: &str,
) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Theora);
    if packets.len() < 3 {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: theora needs 3 header packets, got {}",
            tag,
            packets.len()
        )));
    }
    let ident = oxideav_theora::decode_identification_header(&packets[0]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora ident error: {:?}", tag, e))
    })?;
    if ident.pf != oxideav_theora::PixelFormat::Yuv420 {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: theora only supports 4:2:0 (got {:?})",
            tag,
            ident.pf
        )));
    }
    // comment 包只校验可解析（内容不消费）。
    let _ = oxideav_theora::parse_comment_header(&packets[1]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora comment error: {:?}", tag, e))
    })?;
    let setup = oxideav_theora::decode_setup_header(&packets[2]).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora setup error: {:?}", tag, e))
    })?;
    let mut dec = oxideav_theora::FrameDecoder::new(ident, setup).map_err(|e| {
        crate::error::NyaStatus::_Debug(alloc::format!("{}: theora decoder error: {:?}", tag, e))
    })?;
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    let mut w = 0usize;
    let mut h = 0usize;
    for p in &packets[3..] {
        let d = match dec.decode_frame(p) {
            Ok(d) => d,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let c = match dec.crop_for_display(&d) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        w = c.dims_y.width as usize;
        h = c.dims_y.height as usize;
        if w == 0 || h == 0 {
            skipped += 1;
            continue;
        }
        frames.push(super::Clip::yuv420_to_bgra_vflip(
            &c.samples_y,
            &c.samples_cb,
            &c.samples_cr,
            w,
            h,
        ));
    }

    if frames.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames ({} skipped)",
            tag,
            skipped
        )));
    }
    if skipped > 0 {
        log::warn!(
            "{}: predecoded {} frames, skipped {} bad",
            tag,
            frames.len(),
            skipped
        );
    }

    Ok(EagerClip::eager(w, h, frames))
}

/// raw 序列目录（`frameNNNN.raw`，BGRA 直存）：`want` 封顶预载数，
/// 只收恰好 `frame_bytes` 字节的帧（1920x1080x4，见调用方；半帧不上屏）。
pub(crate) fn from_raw_dir(
    fs: &mut crate::fs::Fs,
    dir: &str,
    want: usize,
    frame_bytes: usize,
    tag: &str,
) -> crate::error::Result<EagerClip> {
    let cdir =
        uefi::CString16::try_from(dir).map_err(|_| crate::error::NyaStatus::FromStrWithBufError)?;
    let entries = match fs.list_dir(&cdir) {
        Ok(e) => e,
        Err(crate::error::NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            return Err(crate::error::NyaStatus::_Debug(alloc::format!(
                "{}: no {} directory",
                tag,
                dir
            )));
        }
        Err(e) => {
            log::warn!("{}: list failed: {:?}", tag, e);
            return Err(e);
        }
    };
    let mut names: Vec<alloc::string::String> = entries
        .into_iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(n, _)| n)
        .filter(|n| n.to_ascii_lowercase().ends_with(".raw"))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no frames in {}",
            tag,
            dir
        )));
    }

    let _ = super::decode_threads(MediaFormat::Raw);
    let mut frames = Vec::new();
    let mut skipped = 0u32;
    for name in names.into_iter().take(want.max(1)) {
        let path = alloc::format!("{}\\{}", dir, name);
        let cstr = match uefi::data_types::CString16::try_from(path.as_str()) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let bytes = match fs.read_file(&cstr) {
            Ok(b) => b,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        // 几何不对（非定值 BGRA）的帧跳过，不让半帧上屏。
        if bytes.len() != frame_bytes {
            skipped += 1;
            continue;
        }
        frames.push(super::bytes_as_pixels(bytes));
    }
    if frames.is_empty() {
        // 全跳过也记一笔（曾静默，QEMU 实测靠它定位到测试帧尺寸写错）。
        log::warn!("{}: no valid frames ({} skipped)", tag, skipped);
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: no valid frames",
            tag
        )));
    }
    if skipped > 0 {
        log::warn!("{}: skipped {} bad frame(s)", tag, skipped);
    }
    // raw 帧几何定值（1920x1080，见调用方 `frame_bytes`）。
    Ok(EagerClip::eager(1920, 1080, frames))
}

/// raws 单文件（`video.raws`，BGRA 帧首尾相接无头）：按 `frame_bytes`
/// 整切，尾部不足一帧即整文件报错（不断言静默跳过，半帧不上屏）。
/// 几何与目录版同定值（1920x1080）。
pub(crate) fn from_raws_bytes(data: &[u8], tag: &str) -> crate::error::Result<EagerClip> {
    let _ = super::decode_threads(MediaFormat::Raw);
    let frame_bytes = 1920usize * 1080 * 4;
    if data.is_empty() || data.len() % frame_bytes != 0 {
        log::warn!(
            "{}: bad raws size {} (want N*{} = N frames)",
            tag,
            data.len(),
            frame_bytes
        );
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "{}: bad raws size {}",
            tag,
            data.len()
        )));
    }
    let mut frames = Vec::with_capacity(data.len() / frame_bytes);
    for chunk in data.chunks_exact(frame_bytes) {
        frames.push(super::bytes_as_pixels(chunk.to_vec()));
    }
    log::info!("{}: predecoded {} raws frames", tag, frames.len());
    Ok(EagerClip::eager(1920, 1080, frames))
}
