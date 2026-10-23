//! 文本上屏（`Screen::draw_str` 及 TUI 定位文本/色块）：整行先在内存光栅，
//! 再一次 `Blt BufferToVideo` 推屏（旧实现逐像素 `VideoFill`，一行几千次 Blt，
//! TUI 整页重画跟不上；改为一段文本一次 Blt）。字形口径见 `crate::font`。
//! 只走 Blt（与旧 draw_str 同引擎），不碰影子环/直写路，播放管线零扰动。

use alloc::vec;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion};

use super::Screen;
use crate::font;

/// 默认前景/背景（`draw_str` 旧口径：白字黑底）。
const FG: BltPixel = BltPixel::new(255, 255, 255);
const BG: BltPixel = BltPixel::new(0, 0, 0);

impl Screen {
    /// 当前模式分辨率（宽, 高）。
    pub fn size(&self) -> (usize, usize) {
        self.gop.current_mode_info().resolution()
    }

    /// 纯色矩形（越屏部分裁掉；零面积直接返回）。
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: BltPixel) {
        let (sw, sh) = self.size();
        if x >= sw || y >= sh {
            return;
        }
        let (w, h) = (w.min(sw - x), h.min(sh - y));
        if w == 0 || h == 0 {
            return;
        }
        let _ = self.gop.blt(BltOp::VideoFill {
            color,
            dest: (x, y),
            dims: (w, h),
        });
    }

    /// 定位单行文本（不换行，超 `max_w` 的字不画）：字格 16 行整块带底色推屏，
    /// 返回实际占宽（px）。TUI 面板/浮层用。
    pub fn draw_text_at(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        fg: BltPixel,
        bg: BltPixel,
        max_w: usize,
    ) -> usize {
        let (sw, sh) = self.size();
        if x >= sw || y >= sh {
            return 0;
        }
        let max_w = max_w.min(sw - x);
        let w = font::str_width_px(text).min(max_w);
        let h = font::CELL_H.min(sh - y);
        if w == 0 || h == 0 {
            return 0;
        }
        let mut buf = vec![bg; w * font::CELL_H];
        let used = font::paint(text, w, |px, py| {
            if px < w {
                buf[py * w + px] = fg;
            }
        });
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: &buf,
            src: BltRegion::Full,
            dest: (x, y),
            dims: (w, h),
        });
        used
    }

    /// 中英文混排输出（旧签名不变）：英文 8x16（旧表字节零改动）+ 非 ASCII 10px
    /// 像素字全量表；超宽自动换行（不拆字），行高 18，文末额外换一行防重叠。
    pub fn draw_str(&mut self, text: &str) {
        let (width, height) = self.size();
        // 防止超出高度
        if self.stdout + 20 >= height {
            self.stdout = 0
        }
        for line in text.split('\n') {
            let mut rest = line;
            loop {
                let n = font::wrap_byte_index(rest, width);
                // 首字就超宽（理论到不了）：强吃一个字防死循环。
                let n = if n == 0 {
                    rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0)
                } else {
                    n
                };
                let (seg, tail) = rest.split_at(n);
                let y = self.stdout;
                self.draw_text_at(0, y, seg, FG, BG, width);
                rest = tail;
                if rest.is_empty() {
                    break;
                }
                self.stdout += font::LINE_H;
                if self.stdout >= height {
                    self.stdout = 0
                }
            }
            self.stdout += font::LINE_H;
            if self.stdout >= height {
                self.stdout = 0
            }
        }
    }
}
