//! 中英文混排度量（纯函数，无 GOP/alloc 以外依赖，host 单测直测本层）。
//!
//! 口径：英文 8px（`latin::FONT_8X16` 一列，像素零改动）；非 ASCII 取 10px 像素字
//! 全量表的自带 advance（中文 10px，比例字），缺字豆腐块 10px；字格高 16、
//! 行高 18px（16 + 2 间隙）。`WP_TOP = MENU_LINES*18 + 8` 只依赖行高，宽度随字体。

use super::cjk;

/// 英文半角宽、高；行高（含 2px 行隙）。
#[allow(dead_code)]
pub const LATIN_W: usize = 8;
#[allow(dead_code)]
pub const LATIN_H: usize = 16;
/// 字格高（英文/中文同高）。
pub const CELL_H: usize = 16;
pub const LINE_H: usize = 18;

/// 单字符占宽：`\n` 为 0（换行不断字）；ASCII 为 8；其余取字体 advance（缺字 10）。
pub fn char_width(ch: char) -> usize {
    if ch == '\n' {
        0
    } else if (ch as u32) < 0x80 {
        LATIN_W
    } else {
        cjk::glyph_or_tofu(ch).adv as usize
    }
}

/// 单行像素宽（`\n` 处截断，不跨行累加；调用方逐行调本函数）。
pub fn str_width_px(s: &str) -> usize {
    let mut w = 0;
    for ch in s.chars() {
        if ch == '\n' {
            break;
        }
        w += char_width(ch);
    }
    w
}

/// 在 `max_px` 内能放下几个字节（不拆字符，`\n` 处截断；供截断与单测共用）。
pub fn wrap_byte_index(s: &str, max_px: usize) -> usize {
    let mut w = 0;
    let mut last = 0;
    for (i, ch) in s.char_indices() {
        if ch == '\n' {
            break;
        }
        let cw = char_width(ch);
        if w + cw > max_px {
            break;
        }
        w += cw;
        last = i + ch.len_utf8();
    }
    last
}
