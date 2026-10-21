use alloc::vec;
use alloc::vec::Vec;
use uefi::Status;
use uefi::proto::console::gop::BltPixel;
use uefi::proto::media::file::{File, FileInfo, RegularFile};

/// 整文件进内存的流式帧源（旧 `staged`/`zero_copy` 管线用：`Clip` 预解码之前的形态，
/// 内存换时间——文件全驻留，逐帧只解码不重复读盘）。
pub struct VideoMemory {
    pub data: Vec<u8>,
    pub cursor: usize,
}

impl VideoMemory {
    pub fn new(mut file: RegularFile) -> crate::error::Result<Self> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                // 其他状态的Option均为None,这里拉平
                Err(e) => Err(e.status())?,
            }
        };
        let size = info.file_size() as usize;

        // 一次性分配内存并读取
        let mut buffer = vec![0u8; size];
        file.read(&mut buffer)?;

        Ok(Self {
            data: buffer,
            cursor: 0,
        })
    }

    /// 取下一帧进 `qoi_buf`（`u32le len + QOI` 串接）。
    /// 越界守卫：尾部不足 4 字节长度头、或长度超出剩余数据时直接结束（不 panic，
    /// 调用方 `rewind` 循环）。
    pub fn next_frame(&mut self, qoi_buf: &mut Vec<u8>) -> bool {
        if self.cursor + 4 > self.data.len() {
            return false;
        }

        let len_bytes = &self.data[self.cursor..self.cursor + 4];
        let frame_len =
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;

        self.cursor += 4;
        let end = self.cursor + frame_len;
        // 长度超出剩余数据：文件截断，按结束处理（不 panic）。
        if end > self.data.len() {
            return false;
        }

        // 将这一帧的数据拷贝到 qoi_buf
        qoi_buf.clear();
        qoi_buf.extend_from_slice(&self.data[self.cursor..end]);

        self.cursor = end;
        true
    }

    pub fn rewind(&mut self) {
        self.cursor = 0;
    }
}

//////// 统一播放剪辑：预解码帧集（`Vec<Vec<BltPixel>>` + 游标）。
//////// 片源七格式（`[play]/[wallpaper] format` 矩阵，见 `config::MediaFormat`）：
//////// - qois：`u32le len + QOI` 串接文件，解码为 BGRA（播放器 P3 老路）；
//////// - raw：`frameNNNN.raw` BGRA 直存序列，零解码直搬（壁纸 P4 老路）；
//////// - hevc：`video.hevc`（Annex-B）→ vendor `rusty_h265` 软解 → YUV420 转 BGRA；
//////// - vp9：`video.ivf`（IVF 容器）→ vendor `vp9dec` 软解 → YUV420 转 BGRA；
//////// - h263/mpeg2v/theora：格式可选、解码后继（选中即明确报错，不硬编解码，
////////   见 `docs/player.md`）。
//////// 壁纸只是薄调：`wallpaper::load` 按开关选格式调这里，循环/上屏走同一套。
pub struct Clip {
    /// 帧几何（qois 取文件头一致值，raw 取首帧/定值；上屏方按此切行）。
    pub w: usize,
    pub h: usize,
    /// 预解码帧（BGRA `BltPixel`，与显存布局一致）。
    pub frames: Vec<Vec<BltPixel>>,
    pub cursor: usize,
}

impl Clip {
    /// qois 字节流解码核心（`tag` 只进串口：`video` / `wallpaper`）。
    /// 坏帧记数跳过，全坏/几何不一致直接报错（曾无限 rewind 空转）。
    pub fn from_qois_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
        let mut frames = Vec::new();
        let mut skipped = 0u32;
        let mut offset = 0;
        let mut expect: Option<(u32, u32)> = None;
        let mut w = 0usize;
        let mut h = 0usize;

        while offset + 4 <= data.len() {
            let len_bytes = &data[offset..offset + 4];
            let frame_len =
                u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                    as usize;

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

        Ok(Self {
            w,
            h,
            frames,
            cursor: 0,
        })
    }

    /// 整文件读入后解码（`video.qois` / `wall.qois`）。
    pub fn from_qois_file(mut file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                Err(e) => return Err(e.status())?,
            }
        };

        let mut buf = vec![0u8; info.file_size() as usize];
        file.read(&mut buf)?;
        Self::from_qois_bytes(&buf, tag)
    }

    /// YUV420（8-bit 全范围）→ BGRA 整数量化（BT.601 近似，
    /// 与 `diag::hevc_probe::show_frame` 同式）：
    /// r = y + (359*v >> 8)；g = y - ((88*u + 183*v) >> 8)；
    /// b = y + (454*u >> 8)，u/v 以 128 为中心。
    /// 奇数几何按 `(w+1)/2` 取色度步进并钳位索引（半帧不上屏由调用方保证）。
    fn yuv420_to_bgra(y: &[u8], u: &[u8], v: &[u8], w: usize, h: usize) -> Vec<BltPixel> {
        let uv_stride = w.div_ceil(2).max(1);
        let mut out = Vec::new();
        if out.try_reserve_exact(w.saturating_mul(h)).is_err() {
            return out;
        }
        out.extend(core::iter::repeat_n(
            BltPixel::new(0, 0, 0),
            w.saturating_mul(h),
        ));
        for row in 0..h {
            for col in 0..w {
                let yy = y
                    .get(row.saturating_mul(w).saturating_add(col))
                    .copied()
                    .unwrap_or(0) as i32;
                let uv_idx = (row / 2).saturating_mul(uv_stride).saturating_add(col / 2);
                let uu = u.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
                let vv = v.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
                let r = (yy + ((359 * vv) >> 8)).clamp(0, 255) as u8;
                let g = (yy - ((88 * uu + 183 * vv) >> 8)).clamp(0, 255) as u8;
                let b = (yy + ((454 * uu) >> 8)).clamp(0, 255) as u8;
                out[row.saturating_mul(w).saturating_add(col)] = BltPixel::new(r, g, b);
            }
        }
        out
    }

    /// hevc 字节流解码核心（`tag` 只进串口）：Annex-B 经 vendor `rusty_h265`
    /// 整流解码（与 `diag::hevc_probe` 同路：`push_annexb` + `flush` + 取帧），
    /// 坏帧计数跳过，全坏/几何不一致直接报错（与 `from_qois_bytes` 同策略）。
    /// 片源：`1080p/video.hevc`（样片见 `src/diag/data/hevc_probe_main.265`）。
    pub fn from_hevc_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
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
            let mut yuv = Vec::new();
            f.write_yuv(&mut yuv);
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
            frames.push(Self::yuv420_to_bgra(ypl, upl, vpl, w, h));
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

        Ok(Self {
            w,
            h,
            frames,
            cursor: 0,
        })
    }

    /// 整文件读入后解码（`video.hevc`，Annex-B 带内 VPS/SPS/PPS）。
    pub fn from_hevc_file(mut file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                Err(e) => return Err(e.status())?,
            }
        };

        let mut buf = vec![0u8; info.file_size() as usize];
        file.read(&mut buf)?;
        Self::from_hevc_bytes(&buf, tag)
    }

    /// vp9 字节流解码核心（`tag` 只进串口）：IVF 容器经 vendor `vp9dec`
    /// 的 `IvfReader` 拆帧 + `Decoder::decode_frame` 逐帧解码，
    /// 只要显示帧（`show_frame == 1`；隐藏 altref 无画面，直接跳过），
    /// 8-bit 4:2:0 才收（其余记数跳过）。坏帧计数跳过，全坏/几何不一致
    /// 直接报错（与 `from_qois_bytes` 同策略）。
    /// 片源：`1080p/video.ivf`（样片见 `src/diag/data/vp9_probe.ivf`，
    /// 160x120×10 帧，host 已验 `shown=10`）。
    pub fn from_vp9_bytes(data: &[u8], tag: &str) -> crate::error::Result<Self> {
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
            for d in decoded {
                // 隐藏帧（altref）无显示画面：不收，不记坏。
                let Some(f) = d.frame else { continue };
                // 只收 8-bit 4:2:0（轮子输出的唯一固件口径；其余记数跳过）。
                if f.bit_depth != 8 || f.subsampling_x != 1 || f.subsampling_y != 1 {
                    skipped += 1;
                    continue;
                }
                let (vp9dec::PlaneData::U8(y), vp9dec::PlaneData::U8(u), vp9dec::PlaneData::U8(v)) =
                    (&f.y, &f.u, &f.v)
                else {
                    skipped += 1;
                    continue;
                };
                match expect {
                    None => {
                        expect = Some((f.width, f.height));
                        w = f.width as usize;
                        h = f.height as usize;
                    }
                    Some((ew, eh)) if (f.width, f.height) != (ew, eh) => {
                        skipped += 1;
                        continue;
                    }
                    _ => {}
                }
                frames.push(Self::yuv420_to_bgra(y, u, v, w, h));
            }
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

        Ok(Self {
            w,
            h,
            frames,
            cursor: 0,
        })
    }

    /// 整文件读入后解码（`video.ivf`，32 字节头 + 12 字节帧头 + 负载）。
    pub fn from_vp9_file(mut file: RegularFile, tag: &str) -> crate::error::Result<Self> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    }
                }
                Err(e) => return Err(e.status())?,
            }
        };

        let mut buf = vec![0u8; info.file_size() as usize];
        file.read(&mut buf)?;
        Self::from_vp9_bytes(&buf, tag)
    }

    /// raw 序列目录（`frameNNNN.raw`，BGRA 直存）：`want` 封顶预载数，
    /// 只收恰好 `frame_bytes` 字节的帧（1920x1080x4，见调用方；半帧不上屏）。
    pub fn from_raw_dir(
        fs: &mut crate::fs::Fs,
        dir: &str,
        want: usize,
        frame_bytes: usize,
        tag: &str,
    ) -> crate::error::Result<Self> {
        let cdir = uefi::CString16::try_from(dir)
            .map_err(|_| crate::error::NyaStatus::FromStrWithBufError)?;
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
            frames.push(bytes_as_pixels(bytes));
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
        Ok(Self {
            w: 1920,
            h: 1080,
            frames,
            cursor: 0,
        })
    }

    /// 极致性能：直接返回当前帧的像素引用，完全无拷贝，无解码。
    pub fn next_frame(&mut self) -> Option<&[BltPixel]> {
        if self.cursor >= self.frames.len() {
            return None;
        }
        let frame = &self.frames[self.cursor];
        self.cursor += 1;
        Some(frame)
    }

    /// 索引取帧（壁纸循环/过渡用，越界回绕）。
    pub fn frame(&self, i: usize) -> &[BltPixel] {
        &self.frames[i % self.frames.len().max(1)]
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn rewind(&mut self) {
        self.cursor = 0;
    }
}

/// BGRA 裸字节零拷贝 reinterpret 为 owned `Vec<BltPixel>`。
/// `BltPixel` 内存布局就是 BGRA（`blue, green, red, reserved`），对齐均为 1，
/// `len % 4 == 0` 已由调用方保证。
fn bytes_as_pixels(bytes: Vec<u8>) -> Vec<BltPixel> {
    debug_assert_eq!(bytes.len() % 4, 0);
    let len = bytes.len() / 4;
    let cap = bytes.capacity() / 4;
    let ptr = bytes.as_ptr() as *mut BltPixel;
    core::mem::forget(bytes);
    unsafe { Vec::from_raw_parts(ptr, len, cap) }
}
