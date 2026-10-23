//! TUI 点阵字体：英文 8x16 + 非 ASCII 10px 像素字全量表 + 中英文混排（UEFI 直写路）。
//!
//! # 轮子优先结论（2026-10-23 重搜，`crates.io` / `docs.rs` 按“no_std CJK bitmap
//! # font / ttf rasterize / pixel font”搜过一轮，不跳步）
//! - 用户拍板：中文直接嵌入 host GUI 同源的 `fusion-pixel-10px-proportional-zh_hans.ttf`
//!   全量（AGENTS 禁止子集化，旧版 Unifont 31 字子集已删）。
//! - `fontdue 0.9.5`：`no_std` 可用，但运行时光栅要浮点 + 堆 + 4MB TTF 原包进
//!   `.efi`；本字体是整像素方块轮廓，构建期光栅即无损。故只作 build-dependency
//!   （根 `build.rs`，host 跑，不进固件运行时），全量 25909 码点烘成位流表
//!   （约 0.6MB，`include_bytes!`），运行时二分查 + 逐位取，零浮点零依赖。
//! - `u8g2-fonts 0.8`（本仓已有依赖）：CJK 档是 wqy/unifont/boutique 等别家字形，
//!   不是 fusion-pixel 同源；且绑 `embedded-graphics` `DrawTarget`。主屏不走它
//!   （仅 `diag/use_embedded_graphics_gop` 演示路保留）。
//! - `unifont` 类：度量 8/16 天然一致但字形不同源（用户要 10px 同源字），不用。
//! - `noto-sans-mono-bitmap 0.3.2`：仅拉丁区，无 CJK，不用。
//!
//! # 度量（`wallpaper::WP_TOP` 只依赖行高，改行高即改布局）
//! - 英文：8px 宽 × 16px 高（`latin::FONT_8X16`，旧表字节零改动，像素不变）。
//! - 非 ASCII：字体自带 advance（中文 10px），字身 9x9 落 16 行字格第 3-11 行
//!   （与英文大写底行齐），缺字 10px 豆腐块。
//! - 行高：18px（16 + 2 间隙），`\n`/超宽换行/文末换行皆 `+= 18`。
//!
//! # 文件拆分（每文件 ≤1000 行，`wc -l` 口径）
//! - `latin.rs`：英文表。`cjk.rs`：全量表查找 + 豆腐块。
//! - `layout.rs`：纯度量/截断。`mod.rs`：本文件（结论 + 光栅入口 + 单测）。

pub mod cjk;
pub mod latin;
pub mod layout;

pub use layout::{CELL_H, LINE_H, char_width, str_width_px, wrap_byte_index};

/// 单行光栅：从笔位 0 起逐字着色，`put(x, y)` 收前景像素（`y` 在 0..16），
/// 超过 `max_px` 的字不画（不拆字）。返回实际占宽。`\n` 视为行尾。
pub fn paint(text: &str, max_px: usize, mut put: impl FnMut(usize, usize)) -> usize {
    let mut pen = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            break;
        }
        let w = char_width(ch);
        if pen + w > max_px {
            break;
        }
        if (ch as u32) < 0x80 {
            let g = &latin::FONT_8X16[(ch as usize) & 0x7F];
            for (y, bits) in g.iter().enumerate() {
                for x in 0..8 {
                    if (bits >> (7 - x)) & 1 == 1 {
                        put(pen + x, y);
                    }
                }
            }
        } else {
            let g = cjk::glyph_or_tofu(ch);
            for y in 0..g.h as usize {
                for x in 0..g.w as usize {
                    let px = pen as isize + g.xmin as isize + x as isize;
                    if px >= 0 && g.on(x, y) {
                        put(px as usize, g.top as usize + y);
                    }
                }
            }
        }
        pen += w;
    }
    pen
}

#[cfg(test)]
mod tests {
    use super::layout::{LATIN_H, LATIN_W};
    use super::{CELL_H, LINE_H, char_width, cjk, latin, paint, str_width_px, wrap_byte_index};
    use alloc::vec::Vec;

    #[test]
    fn latin_a_bytes_match_legacy_table() {
        // 'A'（0x41）旧表字节零改动断言（容差零）。
        let g = latin::FONT_8X16[0x41];
        assert_eq!(
            g,
            [
                0x00, 0x00, 0x10, 0x38, 0x6C, 0xC6, 0xC6, 0xFE, 0xC6, 0xC6, 0xC6, 0xC6, 0x00, 0x00,
                0x00, 0x00
            ]
        );
        assert_eq!(char_width('A'), 8);
    }

    #[test]
    fn cjk_is_full_set_not_subset() {
        // 全量 cmap（25909 码点），任何常用字都在表内（防回退成子集）。
        assert_eq!(cjk::coverage(), 25909);
        for ch in "播放器菜单壁纸引导固件龘齉".chars() {
            assert!(cjk::lookup(ch).is_some(), "{ch} missing");
        }
        // '中'：9x9 字身，advance 10，底行落第 11 行。
        let g = cjk::lookup('中').unwrap();
        assert_eq!((g.adv, g.w, g.h, g.top), (10, 9, 9, 3));
        assert!(g.on(4, 0) && !g.on(0, 0));
    }

    #[test]
    fn mixed_width_is_sum_of_advances() {
        assert_eq!(str_width_px("A中B"), 8 + 10 + 8);
        assert_eq!(str_width_px("中文"), 20);
        assert_eq!(str_width_px(""), 0);
        assert_eq!(str_width_px("A\n中"), 8);
    }

    #[test]
    fn missing_glyph_falls_back_to_tofu_without_line_break() {
        // 私用区 U+E000 不在表内，回豆腐块、占宽 10（不断行）。
        assert!(cjk::lookup('\u{E000}').is_none());
        assert_eq!(char_width('\u{E000}'), 10);
        assert!(cjk::TOFU.on(0, 0) && !cjk::TOFU.on(4, 4));
    }

    #[test]
    fn wrap_never_splits_a_char() {
        assert_eq!(wrap_byte_index("A中B", 18), "A中".len());
        assert_eq!(wrap_byte_index("中文", 8), 0);
        assert_eq!(wrap_byte_index("ABCD", 16), 2);
    }

    #[test]
    fn paint_stays_inside_cell() {
        let mut ys: Vec<usize> = Vec::new();
        let w = paint("A中", 100, |_, y| ys.push(y));
        assert_eq!(w, 18);
        assert!(ys.iter().all(|&y| y < CELL_H));
        assert_eq!((LATIN_W, LATIN_H, CELL_H, LINE_H), (8, 16, 16, 18));
    }
}
