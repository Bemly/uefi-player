//! 有状态解码会话（懒供片的编码侧）：分块保留 + 顺序喂块 + 首帧几何裁决。
//!
//! 只被 `super::lazy::LazyState` 驱动；窗口/策略/重播语义见该文件。

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use uefi::proto::console::gop::BltPixel;

use crate::video::buffer::{BltFrameBuffer, QoiFrameBuffer, RawFrameBuffer};

/// 编码分块 + 有状态解码会话（内存赢在解码帧侧：编码侧保留，分块复用）。
pub(crate) enum Source {
    Qois {
        data: Vec<u8>,
        ranges: Vec<Range<usize>>,
        pos: usize,
    },
    Avc {
        data: Vec<u8>,
        aus: Vec<Range<usize>>,
        pos: usize,
        dec: rusty_h264_decoder::Decoder,
    },
    Hevc {
        data: Vec<u8>,
        aus: Vec<Range<usize>>,
        pos: usize,
        dec: rusty_h265::Decoder,
        pending: VecDeque<Vec<BltPixel>>,
        flushed: bool,
    },
    Vp9Ivf {
        data: Vec<u8>,
        ranges: Vec<Range<usize>>,
        pos: usize,
        dec: vp9dec::Decoder,
        pending: VecDeque<Vec<BltPixel>>,
    },
    Vp9Mp4 {
        frames: Vec<Vec<u8>>,
        pos: usize,
        dec: vp9dec::Decoder,
        pending: VecDeque<Vec<BltPixel>>,
    },
    H263 {
        /// 会话期泄漏（`Box::leak` 常驻，仓内既有风格）：reader 拥有 `&[u8]`
        /// 切片，自引用结构只能借静态片；`reset` 只重建 reader/state，不重泄漏。
        data: &'static [u8],
        reader: h263_rs::parser::H263Reader<&'static [u8]>,
        state: h263_rs::H263State,
    },
    Mpeg2v {
        data: Vec<u8>,
        aus: Vec<Range<usize>>,
        pos: usize,
        dec: filmcraft_mpeg2v::Decoder,
        pending: VecDeque<Vec<BltPixel>>,
        flushed: bool,
        pts: i64,
    },
    Theora {
        packets: Vec<Vec<u8>>,
        pos: usize,
        dec: oxideav_theora::FrameDecoder,
    },
    Raw {
        dir: String,
        names: Vec<String>,
        pos: usize,
        frame_bytes: usize,
    },
}

/// 几何裁决（首帧记录，后继必须一致；与全预解码"混入异形帧跳过"同策略）。
fn check_geom(geom: &mut Option<(usize, usize)>, w: usize, h: usize) -> bool {
    match *geom {
        None => {
            *geom = Some((w, h));
            true
        }
        Some((ew, eh)) => (ew, eh) == (w, h),
    }
}

/// qois 单块解码进复用缓冲（与全预解码同口径：超大帧拒绝 + 几何一致 + R/B 交换）。
fn qoi_into(
    out: &mut Vec<BltPixel>,
    payload: &[u8],
    geom: &mut Option<(usize, usize)>,
    skipped: &mut u32,
) -> bool {
    let header = match qoi::decode_header(payload) {
        Ok(h) => h,
        Err(_) => {
            *skipped += 1;
            return false;
        }
    };
    let n = (header.width * header.height) as usize;
    if n == 0 || n > 16 * 1024 * 1024 {
        *skipped += 1;
        return false;
    }
    if !check_geom(geom, header.width as usize, header.height as usize) {
        *skipped += 1;
        return false;
    }
    out.clear();
    out.resize(n, BltPixel::new(0, 0, 0));
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, n.saturating_mul(4))
    };
    if qoi::decode_to_buf(bytes, payload).is_err() {
        *skipped += 1;
        return false;
    }
    for px in out.iter_mut() {
        core::mem::swap(&mut px.red, &mut px.blue);
    }
    true
}

/// YUV420 平面进复用缓冲（几何已裁决；`vflip` 为 theora 左下原点行序）。
fn yuv_into(
    out: &mut Vec<BltPixel>,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: usize,
    h: usize,
    vflip: bool,
) {
    let n = w.saturating_mul(h);
    out.clear();
    out.resize(n, BltPixel::new(0, 0, 0));
    super::yuv420_to_bgra_into(y, u, v, w, h, out, vflip);
}

/// vp9 单样片喂解码器，产出显示帧全进 `pending`（隐藏 altref/非 420 记数跳过，
/// 与全预解码收集器同口径）。
fn vp9_feed(
    dec: &mut vp9dec::Decoder,
    sample: &[u8],
    geom: &mut Option<(usize, usize)>,
    skipped: &mut u32,
    pending: &mut VecDeque<Vec<BltPixel>>,
) {
    let decoded = match dec.decode_frame(sample) {
        Ok(d) => d,
        Err(_) => {
            *skipped += 1;
            return;
        }
    };
    for d in decoded {
        let Some(f) = d.frame else { continue };
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
        let (w, h) = (f.width as usize, f.height as usize);
        if !check_geom(geom, w, h) {
            *skipped += 1;
            continue;
        }
        pending.push_back(super::Clip::yuv420_to_bgra(y, u, v, w, h));
    }
}

/// hevc 输出队列抽干进 `pending`（只收 8-bit，平面短写记数跳过，与全预解码同口径）。
fn drain_hevc(
    dec: &mut rusty_h265::Decoder,
    geom: &mut Option<(usize, usize)>,
    skipped: &mut u32,
    pending: &mut VecDeque<Vec<BltPixel>>,
) {
    loop {
        let f = match dec.next_frame() {
            Ok(f) => f,
            Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
            Err(_) => {
                *skipped += 1;
                continue;
            }
        };
        if f.bit_depth() != 8 {
            *skipped += 1;
            continue;
        }
        let (w, h) = (f.width, f.height);
        let mut yuv = Vec::new();
        f.write_yuv(&mut yuv);
        let ybytes = w.saturating_mul(h);
        let uvbytes = w.div_ceil(2).saturating_mul(h.div_ceil(2));
        if yuv.len() < ybytes.saturating_add(uvbytes.saturating_mul(2)) {
            *skipped += 1;
            continue;
        }
        let (ypl, rest) = yuv.split_at(ybytes);
        let (upl, vpl) = rest.split_at(uvbytes.min(rest.len()));
        if vpl.len() < uvbytes {
            *skipped += 1;
            continue;
        }
        if !check_geom(geom, w, h) {
            *skipped += 1;
            continue;
        }
        pending.push_back(super::Clip::yuv420_to_bgra(ypl, upl, vpl, w, h));
    }
}

/// mpeg2v 单图进 `pending`（只收 4:2:0 + 平面校验，与全预解码 `push_pic` 同口径）。
fn push_mpeg2v_pic(
    pic: &filmcraft_mpeg2v::Picture,
    geom: &mut Option<(usize, usize)>,
    skipped: &mut u32,
    pending: &mut VecDeque<Vec<BltPixel>>,
) {
    if pic.chroma != filmcraft_mpeg2v::ChromaFormat::Yuv420 {
        *skipped += 1;
        return;
    }
    let (w, h) = (pic.width as usize, pic.height as usize);
    if !check_geom(geom, w, h) {
        *skipped += 1;
        return;
    }
    let yb = w.saturating_mul(h);
    let cb = (w / 2).saturating_mul(h / 2);
    if pic.y.len() < yb || pic.cb.len() < cb || pic.cr.len() < cb {
        *skipped += 1;
        return;
    }
    pending.push_back(super::Clip::yuv420_to_bgra(
        &pic.y[..yb],
        &pic.cb[..cb],
        &pic.cr[..cb],
        w,
        h,
    ));
}

impl Source {
    /// 下一显示帧进 `out`（true = 就绪；false = 流尽；坏块内部记数跳过）。
    /// `geom=None` 为首帧预解（只记录几何），`Some` 后强制一致。
    pub(crate) fn next_into(
        &mut self,
        out: &mut Vec<BltPixel>,
        geom: &mut Option<(usize, usize)>,
        skipped: &mut u32,
    ) -> bool {
        match self {
            Source::Qois { data, ranges, pos } => {
                while *pos < ranges.len() {
                    let r = ranges[*pos].clone();
                    *pos += 1;
                    if qoi_into(out, &data[r], geom, skipped) {
                        return true;
                    }
                }
                false
            }
            Source::Avc {
                data,
                aus,
                pos,
                dec,
            } => {
                while *pos < aus.len() {
                    let r = aus[*pos].clone();
                    *pos += 1;
                    match dec.decode(&data[r]) {
                        Ok(Some(f)) => {
                            // 只收 tight 8-bit 4:2:0（与全预解码同口径）。
                            let planes = f.as_planes();
                            if !planes.is_tight() {
                                *skipped += 1;
                                continue;
                            }
                            let (w, h) = (f.width, f.height);
                            if !check_geom(geom, w, h) {
                                *skipped += 1;
                                continue;
                            }
                            yuv_into(out, planes.y, planes.u, planes.v, w, h, false);
                            return true;
                        }
                        // 参数集/空 AU：无显示帧，继续下一块。
                        Ok(None) => continue,
                        Err(_) => {
                            *skipped += 1;
                            continue;
                        }
                    }
                }
                false
            }
            Source::Hevc {
                data,
                aus,
                pos,
                dec,
                pending,
                flushed,
            } => loop {
                if let Some(px) = pending.pop_front() {
                    *out = px;
                    return true;
                }
                if *pos < aus.len() {
                    let r = aus[*pos].clone();
                    *pos += 1;
                    if dec.push_annexb(&data[r], None).is_err() {
                        *skipped += 1;
                    }
                    drain_hevc(dec, geom, skipped, pending);
                    continue;
                }
                if !*flushed {
                    *flushed = true;
                    dec.flush();
                    drain_hevc(dec, geom, skipped, pending);
                    continue;
                }
                return false;
            },
            Source::Vp9Ivf {
                data,
                ranges,
                pos,
                dec,
                pending,
            } => loop {
                if let Some(px) = pending.pop_front() {
                    *out = px;
                    return true;
                }
                if *pos >= ranges.len() {
                    return false;
                }
                let r = ranges[*pos].clone();
                *pos += 1;
                vp9_feed(dec, &data[r], geom, skipped, pending);
            },
            Source::Vp9Mp4 {
                frames,
                pos,
                dec,
                pending,
            } => loop {
                if let Some(px) = pending.pop_front() {
                    *out = px;
                    return true;
                }
                if *pos >= frames.len() {
                    return false;
                }
                let s = frames[*pos].clone();
                *pos += 1;
                vp9_feed(dec, &s, geom, skipped, pending);
            },
            Source::H263 { reader, state, .. } => loop {
                match state.decode_next_picture(reader) {
                    Ok(()) => {}
                    Err(e) if e.is_eof_error() => return false,
                    // 事务回滚后同字节重试必重错：记数并截断（与全预解码同策略）。
                    Err(_) => {
                        *skipped += 1;
                        return false;
                    }
                }
                let Some(pic) = state.get_last_picture() else {
                    *skipped += 1;
                    continue;
                };
                let Some(dims) = pic.format().into_width_and_height() else {
                    *skipped += 1;
                    continue;
                };
                let (w, h) = (dims.0 as usize, dims.1 as usize);
                if !check_geom(geom, w, h) {
                    *skipped += 1;
                    continue;
                }
                let (y, u, v) = pic.as_yuv();
                if y.len() != w.saturating_mul(h)
                    || u.len() != (w / 2).saturating_mul(h / 2)
                    || v.len() != (w / 2).saturating_mul(h / 2)
                {
                    *skipped += 1;
                    continue;
                }
                yuv_into(out, y, u, v, w, h, false);
                return true;
            },
            Source::Mpeg2v {
                data,
                aus,
                pos,
                dec,
                pending,
                flushed,
                pts,
            } => loop {
                if let Some(px) = pending.pop_front() {
                    *out = px;
                    return true;
                }
                if *pos < aus.len() {
                    let r = aus[*pos].clone();
                    let p = *pts;
                    *pts += 1;
                    *pos += 1;
                    match dec.decode(&data[r], p) {
                        Ok(pics) => {
                            for pic in &pics {
                                push_mpeg2v_pic(pic, geom, skipped, pending);
                            }
                        }
                        Err(_) => {
                            *skipped += 1;
                        }
                    }
                    continue;
                }
                if !*flushed {
                    *flushed = true;
                    for pic in dec.flush() {
                        push_mpeg2v_pic(&pic, geom, skipped, pending);
                    }
                    continue;
                }
                return false;
            },
            Source::Theora { packets, pos, dec } => loop {
                if *pos >= packets.len() {
                    return false;
                }
                let p = packets[*pos].clone();
                *pos += 1;
                let d = match dec.decode_frame(&p) {
                    Ok(d) => d,
                    Err(_) => {
                        *skipped += 1;
                        continue;
                    }
                };
                let c = match dec.crop_for_display(&d) {
                    Ok(c) => c,
                    Err(_) => {
                        *skipped += 1;
                        continue;
                    }
                };
                let (w, h) = (c.dims_y.width as usize, c.dims_y.height as usize);
                if w == 0 || h == 0 {
                    *skipped += 1;
                    continue;
                }
                // 全预解码本不做一致性检查；懒窗口按首帧尺寸复用缓冲，
                // 后继异形帧记数跳过（偏离见本文件头）。
                if !check_geom(geom, w, h) {
                    *skipped += 1;
                    continue;
                }
                yuv_into(out, &c.samples_y, &c.samples_cb, &c.samples_cr, w, h, true);
                return true;
            },
            Source::Raw {
                dir,
                names,
                pos,
                frame_bytes,
            } => {
                while *pos < names.len() {
                    let path = alloc::format!("{}\\{}", dir, names[*pos]);
                    *pos += 1;
                    let cstr = match uefi::CString16::try_from(path.as_str()) {
                        Ok(c) => c,
                        Err(_) => {
                            *skipped += 1;
                            continue;
                        }
                    };
                    let bytes = match crate::fs::Fs::new().and_then(|mut fs| fs.read_file(&cstr)) {
                        Ok(b) => b,
                        Err(_) => {
                            *skipped += 1;
                            continue;
                        }
                    };
                    // 几何不对（非定值 BGRA）的帧跳过（与全预解码同口径）。
                    if bytes.len() != *frame_bytes {
                        *skipped += 1;
                        continue;
                    }
                    // raw 几何定值（1920x1080，构造期已 preset，无需再裁决）。
                    *out = super::bytes_as_pixels(bytes);
                    return true;
                }
                false
            }
        }
    }

    /// staged qois 显式三段（QOI→Raw→Blt，`buffer.rs` 三缓冲复用，
    /// 与旧 `draw_all_mem` 同形状）：终态 BGRA 落 `blt.0`。
    /// 只配 qois 调用（`feed_for` 已裁决），其余变体直接结束。
    pub(crate) fn next_staged(
        &mut self,
        qoi: &mut QoiFrameBuffer,
        raw: &mut RawFrameBuffer,
        blt: &mut BltFrameBuffer,
        geom: &mut Option<(usize, usize)>,
        skipped: &mut u32,
    ) -> bool {
        let (data, ranges, pos) = match self {
            Source::Qois { data, ranges, pos } => (data, ranges, pos),
            _ => {
                debug_assert!(false, "staged only serves qois");
                return false;
            }
        };
        while *pos < ranges.len() {
            let r = ranges[*pos].clone();
            *pos += 1;
            let payload = &data[r];
            // 1. QOI 段：块拷贝进复用缓冲（坏长度/坏头记数跳过）。
            qoi.0.clear();
            qoi.0.extend_from_slice(payload);
            let header = match qoi::decode_header(&qoi.0) {
                Ok(h) => h,
                Err(_) => {
                    *skipped += 1;
                    continue;
                }
            };
            let n = (header.width * header.height) as usize;
            if n == 0 || n > 16 * 1024 * 1024 {
                *skipped += 1;
                continue;
            }
            if !check_geom(geom, header.width as usize, header.height as usize) {
                *skipped += 1;
                continue;
            }
            // 2. Raw 段：解码进复用像素缓冲（容量不足按需扩，与旧循环同语义）。
            let header = loop {
                match qoi::decode_to_buf(&mut raw.pixels, &qoi.0) {
                    Ok(h) => break Some(h),
                    Err(qoi::Error::OutputBufferTooSmall { required, .. }) => {
                        raw.pixels.resize(required, 0)
                    }
                    Err(_) => {
                        *skipped += 1;
                        break None;
                    }
                }
            };
            let Some(header) = header else { continue };
            raw.header = header;
            if raw.get_size() != n {
                *skipped += 1;
                continue;
            }
            // 3. Blt 段：RGBA→BGRA 进复用上屏缓冲。
            let step = raw.header.channels.as_u8() as usize;
            if blt.0.len() < n {
                blt.0.resize(n, BltPixel::new(0, 0, 0));
            }
            raw.pixels[..n.saturating_mul(step)]
                .chunks_exact(step)
                .zip(blt.0[..n].iter_mut())
                .for_each(|(r, b)| {
                    b.red = r[0];
                    b.green = r[1];
                    b.blue = r[2];
                });
            return true;
        }
        false
    }

    /// 重建解码器状态（`rewind` 用）：编码保留不动，会话归零重建。
    /// theora 重解三头（必成功：构造期已验；失败则耗尽下标，温和结束）。
    pub(crate) fn reset(&mut self) {
        match self {
            Source::Qois { pos, .. } => *pos = 0,
            Source::Avc { pos, dec, .. } => {
                *pos = 0;
                *dec = rusty_h264_decoder::Decoder::new();
            }
            Source::Hevc {
                pos,
                dec,
                pending,
                flushed,
                ..
            } => {
                *pos = 0;
                *dec = rusty_h265::Decoder::new();
                pending.clear();
                *flushed = false;
            }
            Source::Vp9Ivf {
                pos, dec, pending, ..
            }
            | Source::Vp9Mp4 {
                pos, dec, pending, ..
            } => {
                *pos = 0;
                *dec = vp9dec::Decoder::new();
                pending.clear();
            }
            Source::H263 {
                data,
                reader,
                state,
            } => {
                *reader = h263_rs::parser::H263Reader::from_source(*data);
                *state = h263_rs::H263State::new(h263_rs::DecoderOption::empty());
            }
            Source::Mpeg2v {
                pos,
                dec,
                pending,
                flushed,
                pts,
                ..
            } => {
                *pos = 0;
                *dec = filmcraft_mpeg2v::Decoder::new();
                pending.clear();
                *flushed = false;
                *pts = 0;
            }
            Source::Theora { packets, pos, dec } => {
                *pos = 3.min(packets.len());
                // 三头重解（构造期已验，必成功；万一失败则耗尽下标温和结束）。
                let rebuilt = match oxideav_theora::decode_identification_header(&packets[0]) {
                    Ok(ident) => match oxideav_theora::parse_comment_header(&packets[1]) {
                        Ok(_) => match oxideav_theora::decode_setup_header(&packets[2]) {
                            Ok(setup) => oxideav_theora::FrameDecoder::new(ident, setup).ok(),
                            Err(_) => None,
                        },
                        Err(_) => None,
                    },
                    Err(_) => None,
                };
                match rebuilt {
                    Some(d) => *dec = d,
                    None => *pos = packets.len(),
                }
            }
            Source::Raw { pos, .. } => *pos = 0,
        }
    }
}
