//! 中英文混排度量（纯函数，无 GOP/alloc 以外依赖，host 单测直测本层）。
//!
//! 口径冻结：英文 8px（`latin::FONT_8X16` 一列）、中文/缺字 16px 全角、
//! 行高 18px（16 + 2 间隙）。`WP_TOP = MENU_LINES*18 + 8` 与菜单布局依赖
//! 此口径，本层改宽即改屏布局，故宽表只增不改。

use super::cjk;
use super::latin::FONT_8X16;

/// 英文半角宽、高；中文全角宽、高；行高（含 2px 行隙）。
#[allow(dead_code)]
pub const LATIN_W: usize = 8;
#[allow(dead_code)]
pub const LATIN_H: usize = 16;
#[allow(dead_code)]
pub const CJK_W: usize = 16;
#[allow(dead_code)]
pub const CJK_H: usize = 16;
pub const LINE_H: usize = 18;

/// 是否按全角占宽：CJK 统一表意（4E00-9FFF）+ CJK 标点（3000-303F）+
/// 全角形（FF00-FFEF）+ 日文假名（3040-30FF）按 16px 走；其余非 ASCII
/// 缺字亦按 16px 豆腐块走（不断行、保对齐）。
#[allow(dead_code)]
pub fn is_wide(ch: char) -> bool {
    let v = ch as u32;
    (0x4E00..=0x9FFF).contains(&v)
        || (0x3000..=0x303F).contains(&v)
        || (0xFF00..=0xFFEF).contains(&v)
        || (0x3040..=0x30FF).contains(&v)
        || v >= 0x80
}

/// 单字符占宽：`\n` 为 0（换行不断字）；ASCII 为 8；其余为 16。
pub fn char_width(ch: char) -> usize {
    if ch == '\n' {
        0
    } else if (ch as u32) < 0x80 {
        LATIN_W
    } else {
        CJK_W
    }
}

/// 单行像素宽（`\n` 按换行计 0，不跨行累加；调用方逐行调本函数）。
#[allow(dead_code)]
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

/// 在 `max_px` 内能放下几个字节（不拆字符，`\n` 处截断；供换行与单测共用）。
#[allow(dead_code)]
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

/// 统一 16 行位点（`u16` 行，bit15 在左）：ASCII 占左 8 列（原字节左移 8，
/// 右 8 清零，与 CJK 左缘对齐）；中文取子集；缺字取豆腐块；`\n` 取空。
/// 单测逐字节断言本函数（容差零），`Screen` 画屏路与本函数同源（同表同位序）。
#[allow(dead_code)]
pub fn glyph_rows_16(ch: char) -> [u16; 16] {
    if ch == '\n' || ch == '\r' {
        return [0u16; 16];
    }
    if (ch as u32) < 0x80 {
        let g = FONT_8X16[(ch as usize) & 0x7F];
        let mut out = [0u16; 16];
        for (i, b) in g.iter().enumerate() {
            out[i] = (*b as u16) << 8;
        }
        return out;
    }
    let rows = cjk::rows_or_tofu(ch);
    *rows
}
