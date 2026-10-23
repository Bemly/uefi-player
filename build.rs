//! 构建期（host 侧）把 10px 中文像素字体全量转成固件点阵表（`src/font/cjk.rs` 读）。
//!
//! # 轮子优先结论（2026-10-23，按“ttf rasterize / pixel font bitmap / no_std”搜过）
//! - 字体同源 host GUI：`tools/config-gui/assets/fusion-pixel-10px-proportional-zh_hans.ttf`
//!   （OFL-1.1，许可证见同目录 `OFL.txt`），AGENTS 明令禁止子集化，故此处**全量**
//!   烘焙 cmap 里的每一个码点（25909 个，不挑字、不裁剪）。
//! - 固件运行时不做 TTF 光栅：`fontdue`/`ab_glyph` 运行时都要浮点 + 堆 + 4MB 原包进
//!   `.efi`，换不来像素字的收益（像素字的轮廓本就是整像素方块）。
//! - 构建期光栅用 `fontdue 0.9`（build-dependency，只在 host 跑，不进固件运行时；
//!   MIT/Apache/Zlib，维护活跃，MSRV 远低于 1.92）：10px 正好是该字体的设计 em
//!   （upem 1000 = 100 单位/像素），实测 25909 字覆盖率全是 0/255（无半透明像素），
//!   阈值 128 即逐像素无损。`ttf-parser`/`ab_glyph` 只给轮廓或同质光栅，不再多引。
//!
//! # 产物（`OUT_DIR`，`include_bytes!` 进固件）
//! - `cjk_index.bin`：每字 12 字节，按码点升序（二分查）：
//!   `[0..3]` 码点 u24 LE、`[3]` advance、`[4..8]` 位流字节偏移 u32 LE、
//!   `[8]` xmin（i8）、`[9]` 顶行（16 行字格内的行号）、`[10]` 宽、`[11]` 高。
//! - `cjk_bits.bin`：逐字位流（每字从字节边界起，行优先，`y*w+x` 位，MSB 先）。
//! - 字格：16 行（与英文 8x16 同高）。该字体中文字身是基线下探 1px 的 9x9
//!   （`ymin=-1`），`BASELINE=11` 让中文底行落第 11 行，与英文 8x16 大写底行同行，
//!   混排视觉齐底；越出 0..16 的行裁掉。

use std::env;
use std::fs;
use std::path::PathBuf;

/// 字体设计像素尺寸（10px 档，即该字体的 em）。
const PX: f32 = 10.0;
/// 字格高（与英文 8x16 一致）。
const CELL_H: i32 = 16;
/// 基线行：字形 `ymin=0` 的像素行落在第 `BASELINE - 1` 行（中文 `ymin=-1` 底行即第 11 行）。
const BASELINE: i32 = 11;

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ttf = root.join("tools/config-gui/assets/fusion-pixel-10px-proportional-zh_hans.ttf");
    println!("cargo:rerun-if-changed={}", ttf.display());
    println!("cargo:rerun-if-changed=build.rs");

    let data = fs::read(&ttf).expect("read fusion-pixel ttf");
    let font = fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
        .expect("parse fusion-pixel ttf");

    let mut chars: Vec<(char, u16)> = font.chars().iter().map(|(c, i)| (*c, i.get())).collect();
    chars.sort_unstable_by_key(|(c, _)| *c as u32);

    let mut index: Vec<u8> = Vec::with_capacity(chars.len() * 12);
    let mut bits: Vec<u8> = Vec::new();
    for (ch, gi) in chars {
        let (m, cov) = font.rasterize_indexed(gi, PX);
        // 顶行 = 基线 - 字形上缘；越界行裁掉（极少数大字形/组合符）。
        let top = BASELINE - (m.ymin + m.height as i32);
        let y0 = (-top).max(0);
        let y1 = (m.height as i32).min(CELL_H - top);
        let h = (y1 - y0).max(0);
        let w = m.width as i32;
        let off = bits.len() as u32;
        let mut acc: u8 = 0;
        let mut n = 0;
        for y in y0..y0 + h {
            for x in 0..w {
                let on = cov[(y * w + x) as usize] >= 128;
                acc = (acc << 1) | on as u8;
                n += 1;
                if n == 8 {
                    bits.push(acc);
                    acc = 0;
                    n = 0;
                }
            }
        }
        if n > 0 {
            bits.push(acc << (8 - n));
        }
        let cp = ch as u32;
        index.extend_from_slice(&cp.to_le_bytes()[..3]);
        index.push(m.advance_width.round().clamp(0.0, 255.0) as u8);
        index.extend_from_slice(&off.to_le_bytes());
        index.push(m.xmin.clamp(-128, 127) as i8 as u8);
        index.push((top + y0).clamp(0, CELL_H) as u8);
        index.push(if h == 0 { 0 } else { w.min(255) as u8 });
        index.push(h as u8);
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("cjk_index.bin"), &index).unwrap();
    fs::write(out.join("cjk_bits.bin"), &bits).unwrap();
}
