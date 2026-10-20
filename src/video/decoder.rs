use alloc::vec;
use alloc::vec::Vec;
use uefi::Status;
use uefi::proto::console::gop::BltPixel;
use uefi::proto::media::file::{File, FileInfo, RegularFile};

//////// 统一播放剪辑：预解码帧集（`Vec<Vec<BltPixel>>` + 游标）。
//////// 片源双格式（`[play]/[wallpaper] format` 矩阵，见 `config::MediaFormat`）：
//////// - qois：`u32le len + QOI` 串接文件，解码为 BGRA（播放器 P3 老路）；
//////// - raw：`frameNNNN.raw` BGRA 直存序列，零解码直搬（壁纸 P4 老路）。
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
