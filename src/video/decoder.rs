use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use uefi::proto::console::gop::BltPixel;
use uefi::proto::media::file::{File, FileInfo, RegularFile};
use uefi::Status;

///////// 全部写入内存
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
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL =>
                    if let Some(size) = *e.data() {
                        info_buf.resize(size, 0)
                    },
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

    /// 模仿之前的 read_frame_next，但改为从内存切片
    pub fn next_frame(&mut self, qoi_buf: &mut Vec<u8>) -> bool {
        // 越界守卫：尾部不足 4 字节长度头时直接结束（此前直切会 panic）。
        if self.cursor + 4 > self.data.len() {
            return false;
        }

        // 假设你的每帧 QOI 数据前面有 4 字节长度信息（这是常见的拼接方式）
        let len_bytes = &self.data[self.cursor..self.cursor + 4];
        let frame_len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;

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

//////// 原始数据全缓存
pub struct VideoMemoryRaw {
    // 存储所有帧的像素数据，每一项都是一帧完整的 BltPixel 数组
    pub frames: Vec<Vec<BltPixel>>,
    pub cursor: usize,
}

impl VideoMemoryRaw {
    pub fn new(mut file: RegularFile) -> crate::error::Result<Self> {
        let mut info_buf = vec![0u8; 128];
        let info = loop {
            match file.get_info::<FileInfo>(&mut info_buf) {
                Ok(info) => break info,
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                    if let Some(size) = *e.data() { info_buf.resize(size, 0) }
                }
                Err(e) => return Err(e.status())?,
            }
        };

        let mut compressed_buffer = vec![0u8; info.file_size() as usize];
        file.read(&mut compressed_buffer)?;

        // 预解码。错位闭环（P3）：坏帧只记数跳过，但全坏/几何不一致直接报错，
        // 不再静默进“无限 rewind 空转”（此前空集会导致 blt_loop 永远打空转）。
        let mut frames = Vec::new();
        let mut skipped = 0u32;
        let mut offset = 0;
        let mut expect: Option<(u32, u32)> = None;

        while offset + 4 <= compressed_buffer.len() {
            // 读取长度信息
            let len_bytes = &compressed_buffer[offset..offset + 4];
            let frame_len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;

            let data_start = offset + 4;
            let next_frame_pos = data_start + frame_len;

            if next_frame_pos > compressed_buffer.len() { break }

            let frame_data = &compressed_buffer[data_start..next_frame_pos];

            // 解码这一帧
            // 我们先解码头来获取分辨率，或者假设你已知分辨率
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
                // 真机错位多半落在这里：记数跳过，阈值见文末（曾无限静默丢帧）。
                Err(_) => {
                    skipped += 1;
                    offset = next_frame_pos;
                    continue;
                }
            };
            // 同一文件内几何必须一致（混入异形帧会撕裂后面所有帧定位）。
            match expect {
                None => expect = Some((header.width, header.height)),
                Some((w, h)) if (header.width, header.height) != (w, h) => {
                    skipped += 1;
                    offset = next_frame_pos;
                    continue;
                }
                _ => {}
            }

            let mut pixel_buffer = vec![BltPixel::new(0, 0, 0); pixel_count];

            // 完成解码，存入 pixel_buffer
            if qoi::decode_to_buf(
                unsafe { core::slice::from_raw_parts_mut(pixel_buffer.as_mut_ptr() as *mut u8, pixel_count * 4) },
                frame_data
            ).is_err() {
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
                "video: no valid frames ({} skipped)",
                skipped
            )));
        }
        if skipped > 0 {
            log::warn!("video: predecoded {} frames, skipped {} bad", frames.len(), skipped);
        }

        Ok(Self {
            frames,
            cursor: 0,
        })
    }

    /// 极致性能：直接返回当前帧的像素引用，完全无拷贝，无解码
    pub fn next_frame(&mut self) -> Option<&[BltPixel]> {
        if self.cursor >= self.frames.len() {
            return None;
        }
        let frame = &self.frames[self.cursor];
        self.cursor += 1;
        Some(frame)
    }

    pub fn rewind(&mut self) {
        self.cursor = 0;
    }
}