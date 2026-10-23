//! TUI 点阵字体：英文 8x16 + 中文 16x16 子集 + 中英文混排（UEFI 直写路）。
//!
//! # 轮子优先结论（2026-10-23，`crates.io` / `docs.rs` 按“no_std CJK bitmap
//! # font / 中文点阵 / pixel font”搜过一轮，不跳步）
//! - `u8g2-fonts 0.8`（本仓已有依赖）：有 CJK 位点（`wqy12/13/14/15/16_*_gb2312*`
//!   约 100-300KB/档、`unifont_t_chinese1/2/3`、`gb16st_t_*`、`gb24st_t_*`、
//!   `boutique_bitmap_7x7/9x9_*_chinese*/gb2312*`，已实锤 `src/fonts/` 下 1999 个
//!   `.u8g2font`）。MSRV 1.81（本仓线 1.92 可用）、`#![no_std]`（仅依赖
//!   `embedded-graphics-core`，`default-features=false` 即去 `embedded-graphics`
//!   仍可用 `FontRenderer`）、维护活跃（2026-05-24 发 0.8.0）、下载量级高（嵌入式
//!   生态常用）。不用：①变宽比例字（`render_aligned` 按字形宽推，与本仓 8/16 固定
//!    advance + 18px 行高 + `WP_TOP` 布局不兼容，换字即改全屏布局）；②渲染器绑定
//!   `embedded-graphics` `DrawTarget`（本仓 `Screen::draw_str` 是 GOP `Blt` 逐像素
//!   直写，另起 `DrawTarget` 适配层徒增调用）；③字体本身非 MIT/Apache（`u8g2` 上游
//!   `LICENSE` 另算，法务口径不如 Unifont 子集干净）；④单档体积百 KB 级（菜单几十
//!   字用不上全档）。本仓 `u8g2-fonts` 保留给 `diag/use_embedded_graphics_gop`
//!   演示路（`boutique_bitmap_7x7_t_chinese3`），主屏 `draw_str` 不走它。
//! - `unifont` 类（`unifont 1.1.0+data-15.1.05` / `unifont-bitmap` /
//!   `libbaremetal-unifont`）：半角 8x16 + 全角 16x16，度量与本仓冻结口径天然一致；
//!   `unifont` 本体 `#![no_std]`、MIT、`default-features=false` 无碍（零依赖，构建期
//!   `build.rs` 解析 `unifont.hex`）；维护中等（跟随 Unifont 15.x 数据）、下载量级中。
//!   不用全量：57k 字形约 1.3MB+ 静态表（`GLYPH_TABLE` 全进 `.efi`，固件体积预算炸），
//!   且无官方子集特性（要么全要要么自裁）。本模块改为“子集转写”：仅收录 TUI 常用 31 字
//!   （`cjk.rs`，约 1KB，来源同为 Unifont 15.1.05，见下），度量同源、体积可控。
//! - `zpix` 类（最像素 `zpix-pixel-font`）：GitHub TTF/像素字（OFL），`crates.io`
//!   无 Rust 位点箱（`cargo search zpix` 零命中），要自 vendor TTF + 自写解析/光栅，
//!   无 `no_std`/`alloc` 现成口径；且 TTF 向量路与本仓“预烘位点直写”形态不合。不用。
//! - `fontdue 0.9.5` + 子集化：`no_std` 可用（`--no-default-features` 去 `simd`，
//!   `alloc` 即走堆线），MIT/Apache/Zlib 三选，维护活跃、下载量级高。但要吃 TTF
//!   原包（host 侧 `fusion-pixel-10px` 约 4MB，全量进固件不可接受；子集需 `build.rs`
//!   预光栅 + 体积核算），且运行时光栅float/alloc 开销换不来菜单几十字的收益。
//!   不用（固件侧允许子集、体积进预算——本模块 1KB 子集即该预算的极小解；若日后要全量
//!   CJK，再立项 `build.rs` 预光栅 + `fontdue` 子集，不在本 P1）。
//! - `noto-sans-mono-bitmap 0.3.2`：`no_std`、MSRV 1.56、无 alloc 无浮点，
//!   但仅拉丁区（`unicode-basic-latin` 等），无 CJK，不用。
//! 结论一句话：英文 8x16 原表冻结零改动，中文取 Unifont 15.1.05 的 31 字 16x16
//! 子集转写（OFL-1.1/GPL-2+ 嵌入例外，固件侧允许子集），无新增依赖，度量 8/16/18
//! 对齐 `WP_TOP`，缺字豆腐块不断行。
//!
//! # 度量冻结（`wallpaper::WP_TOP` 依赖，改宽即改布局）
//! - 英文：8px 宽 × 16px 高（`latin::FONT_8X16`，旧 `video/ascii_font.rs` 字节零改动迁移）。
//! - 中文/缺字：16px 宽 × 16px 高全角（含豆腐块）。
//! - 行高：18px（16 + 2 间隙），`\n`/超宽换行/文末换行皆 `+= 18`（旧 `draw_str` 语义原样保留）。
//! - `draw_str` 签名保持 `pub fn draw_str(&mut self, s: &str)` 不变。
//!
//! # 文件拆分（每文件 ≤1000 行，`wc -l` 口径）
//! - `latin.rs`：英文表（约 520 行）。
//! - `cjk.rs`：中文子集 + 豆腐块 + 查表（约 60 行）。
//! - `layout.rs`：纯度量/换行/统一位点（约 110 行，无 GOP，可 host 单测）。
//! - `mod.rs`：本文件（结论 + 转发 + 单测，约 350 行）。

pub mod cjk;
pub mod latin;
pub mod layout;

// 生产路仅用行高 + 占宽 + 查表；其余度量 helper 供单测/后续 TUI，允许非测试构建未引用。
#[allow(unused_imports)]
pub use layout::{LINE_H, char_width};

/// 中文位点或豆腐块（`Screen` 非 ASCII 路统一经此，永不回空）。
pub fn cjk_glyph_or_tofu(ch: char) -> &'static [u16; 16] {
    cjk::rows_or_tofu(ch)
}

#[cfg(test)]
mod tests {
    use super::char_width;
    use super::cjk;
    use super::latin;
    use super::layout::{CJK_H, CJK_W, LATIN_H, LATIN_W, LINE_H};
    use super::layout::{glyph_rows_16, is_wide, str_width_px, wrap_byte_index};
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
        // 统一位点把 ASCII 左对齐进 u16 高 8 位（右 8 清零）。
        let rows = glyph_rows_16('A');
        assert_eq!(rows[2], 0x1000);
        assert_eq!(rows[7], 0xFE00);
        assert_eq!(char_width('A'), 8);
        assert!(!is_wide('A'));
    }

    #[test]
    fn cjk_zhong_bytes_match_unifont_subset() {
        // '中' U+4E2D 取自 Unifont 15.1.05 子集（容差零，逐字节）。
        let rows = cjk::lookup('中').expect("subset must contain 中");
        assert_eq!(
            *rows,
            [
                0x0100, 0x0100, 0x0100, 0x0100, 0x3FF8, 0x2108, 0x2108, 0x2108, 0x2108, 0x2108,
                0x3FF8, 0x2108, 0x0100, 0x0100, 0x0100, 0x0100
            ]
        );
        assert_eq!(glyph_rows_16('中'), *rows);
        assert_eq!(char_width('中'), 16);
        assert!(is_wide('中'));
    }

    #[test]
    fn mixed_width_is_sum_of_advances() {
        // "A中B" = 8 + 16 + 8 = 32（混排占宽口径）。
        assert_eq!(str_width_px("A中B"), 32);
        // 全中文 "中文" = 16 + 16 = 32。
        assert_eq!(str_width_px("中文"), 32);
        // 空串为 0，换行处截断。
        assert_eq!(str_width_px(""), 0);
        assert_eq!(str_width_px("A\n中"), 1 * 8);
    }

    #[test]
    fn missing_glyph_falls_back_to_tofu_without_line_break() {
        // '龘'（U+9F98）不在 31 字子集内，回豆腐块、占宽仍 16（不断行）。
        assert!(cjk::lookup('龘').is_none());
        let rows = glyph_rows_16('龘');
        assert_eq!(rows, cjk::TOFU_ROWS);
        assert_eq!(rows[0], 0xFFFF);
        assert_eq!(rows[1], 0x8001);
        assert_eq!(rows[15], 0xFFFF);
        assert_eq!(char_width('龘'), 16);
        assert_eq!(str_width_px("A龘B"), 8 + 16 + 8);
    }

    #[test]
    fn wrap_never_splits_a_char() {
        // max 24px 时 "A中B" 只能放下 "A中"（8+16），不拆 CJK。
        assert_eq!(wrap_byte_index("A中B", 24), "A中".len());
        // max 8px 时 "中文" 一个都放不下首字之外的半个（回 0 而非半字）。
        assert_eq!(wrap_byte_index("中文", 8), 0);
        assert_eq!(wrap_byte_index("中文", 16), "中".len());
        // 纯英文按 8px 切分。
        assert_eq!(wrap_byte_index("ABCD", 16), 2);
    }

    #[test]
    fn fullwidth_punct_counts_as_wide() {
        // 全角逗号 U+FF0C 按全角占宽（标点不断行错位）。
        assert!(is_wide('，'));
        assert_eq!(char_width('，'), 16);
        // 半角逗号仍 8px。
        assert!(!is_wide(','));
        assert_eq!(char_width(','), 8);
    }

    #[test]
    fn line_bitmap_mixed_rows_are_byte_exact() {
        // 混排单行位图逐行断言：取 'A' 第 2 行 + '中' 第 4 行拼行宽语义。
        // 'A' 行2 = 0x10（左在高位），'中' 行4 = 0x3FF8（满幅横）。
        let a = glyph_rows_16('A');
        let zhong = glyph_rows_16('中');
        assert_eq!(a[2], 0x1000);
        assert_eq!(zhong[4], 0x3FF8);
        // 行高/字宽常量冻结（布局回归网）。
        assert_eq!(
            (LATIN_W, LATIN_H, CJK_W, CJK_H, LINE_H),
            (8, 16, 16, 16, 18)
        );
        // 覆盖集字数（增字需同步改此断言，防无声膨胀）。
        assert_eq!(cjk::coverage(), 31);
        // 渲染行宽向量自检（纯逻辑，不碰 GOP，host 可跑）。
        let mut widths: Vec<usize> = Vec::new();
        for ch in "播A放".chars() {
            widths.push(char_width(ch));
        }
        assert_eq!(widths, [16, 8, 16]);
    }
}
