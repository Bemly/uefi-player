use alloc::vec;
use alloc::vec::Vec;
use qoi::Header;
use uefi::proto::console::gop::BltPixel;

/// 3阶段: Qoi -> Raw -> Blt
/// Qoi压缩的数据
pub struct QoiFrameBuffer(pub Vec<u8>);

/// 解码后的数据
pub struct RawFrameBuffer {
    pub pixels: Vec<u8>,
    pub header: Header,
}

/// 交给GOP的数据
pub struct BltFrameBuffer(pub Vec<BltPixel>);

impl QoiFrameBuffer {
    pub fn new(size: usize) -> Self {
        Self { 0: vec![0u8; size] }
    }
}

impl RawFrameBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            pixels: vec![0u8; size],
            header: Default::default(),
        }
    }

    #[inline]
    pub fn get_size(&self) -> usize {
        (self.header.width * self.header.height) as usize
    }
}

impl BltFrameBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            0: vec![BltPixel::new(0, 0, 0); size],
        }
    }
}
