//! 中文（及一切非 ASCII）10px 像素字全量点阵 + 豆腐块。
//!
//! 来源：`tools/config-gui/assets/fusion-pixel-10px-proportional-zh_hans.ttf`
//! （与 host GUI 同源，OFL-1.1，见同目录 `OFL.txt`），构建期 `build.rs` 全量烘焙
//! cmap 全部 25909 码点（禁止子集化，AGENTS 口径），`include_bytes!` 进固件。
//! 格式见 `build.rs` 头注释：12 字节索引（按码点升序，二分查）+ 逐字位流。
//! 比例字：advance 取字体自带（中文 10px，西文附加符号 4-8px 不等），
//! 字形落在 16 行字格内（与英文 8x16 同高、底行齐），缺字回豆腐块，不断行、不 panic。

/// 全量索引（12 字节/字）。
static INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cjk_index.bin"));
/// 全量位流。
static BITS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cjk_bits.bin"));

/// 索引记录字节数。
const REC: usize = 12;

/// 单字点阵视图（位流借全局表，零拷贝）。
#[derive(Clone, Copy)]
pub struct Glyph {
    /// 前进宽（px）。
    pub adv: u8,
    /// 字形左缘相对笔位的偏移（px，可负）。
    pub xmin: i8,
    /// 字形顶行（16 行字格内的行号）。
    pub top: u8,
    pub w: u8,
    pub h: u8,
    bits: &'static [u8],
}

impl Glyph {
    /// 字形内 `(x, y)` 是否着色（越界为否）。
    pub fn on(&self, x: usize, y: usize) -> bool {
        if x >= self.w as usize || y >= self.h as usize {
            return false;
        }
        let i = y * self.w as usize + x;
        self.bits
            .get(i / 8)
            .map(|b| (b >> (7 - i % 8)) & 1 == 1)
            .unwrap_or(false)
    }
}

/// 缺字豆腐块：9x9 空心框，advance 10（与中文字身同尺寸，不断行、保对齐）。
static TOFU_BITS: [u8; 11] = tofu_bits();
pub const TOFU: Glyph = Glyph {
    adv: 10,
    xmin: 0,
    top: 3,
    w: 9,
    h: 9,
    bits: &TOFU_BITS,
};

const fn tofu_bits() -> [u8; 11] {
    let mut out = [0u8; 11];
    let mut y = 0;
    while y < 9 {
        let mut x = 0;
        while x < 9 {
            if y == 0 || y == 8 || x == 0 || x == 8 {
                let i = y * 9 + x;
                out[i / 8] |= 0x80 >> (i % 8);
            }
            x += 1;
        }
        y += 1;
    }
    out
}

/// 全量字数（25909 = cmap 全集，供自检/串口）。
pub fn coverage() -> usize {
    INDEX.len() / REC
}

/// 码点 → 点阵；表内无此字回 `None`（二分查）。
pub fn lookup(ch: char) -> Option<Glyph> {
    let want = ch as u32;
    let (mut lo, mut hi) = (0usize, coverage());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let r = &INDEX[mid * REC..mid * REC + REC];
        let cp = r[0] as u32 | (r[1] as u32) << 8 | (r[2] as u32) << 16;
        if cp == want {
            let off = u32::from_le_bytes([r[4], r[5], r[6], r[7]]) as usize;
            return Some(Glyph {
                adv: r[3],
                xmin: r[8] as i8,
                top: r[9],
                w: r[10],
                h: r[11],
                bits: BITS.get(off..).unwrap_or(&[]),
            });
        }
        if cp < want {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

/// 查表或豆腐块（`Screen` 非 ASCII 路统一经此，永不回空）。
pub fn glyph_or_tofu(ch: char) -> Glyph {
    lookup(ch).unwrap_or(TOFU)
}
