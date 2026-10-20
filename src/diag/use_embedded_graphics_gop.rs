//! embedded-graphics 中文/日文点阵演示（菜单 6）。
//!
//! 原先用 `embedded-graphics-gop::BltDrawTarget`，但该 crate 最新版(0.4.2)仍钉死
//! uefi 0.36，与本仓库的 uefi 0.41 类型不兼容（图里出现两套 uefi）。这里自己实现
//! 一个最小 `DrawTarget`：画到内存画布，最后一次性 `blt(BufferToVideo)` 上屏。

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::jis_x0201::FONT_10X20;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts::u8g2_font_boutique_bitmap_7x7_t_chinese3;
use uefi::boot;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};

use crate::error::{NyaStatus, Result};

/// 内存画布：把 embedded-graphics 画的内容先落在 RAM，再整体上屏。
struct Canvas {
    w: usize,
    h: usize,
    buf: Vec<BltPixel>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            buf: vec![BltPixel::new(0, 0, 0); w * h],
        }
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(self.w as u32, self.h as u32)
    }
}

impl DrawTarget for Canvas {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> core::result::Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(Point { x, y }, color) in pixels {
            if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
                continue;
            }
            // GOP 是 BGRA 内存序；BltPixel::new(red, green, blue) 负责字段摆放。
            self.buf[y as usize * self.w + x as usize] =
                BltPixel::new(color.r(), color.g(), color.b());
        }
        Ok(())
    }
}

/// 特例：需要自己独占 GOP（含强制切 800x600），所以签名是 `run() -> Result`，
/// 由菜单先 `drop(Screen)` 再调用，跑完重建 `Screen`。见 [`crate::diag::registry::Entry`]。
pub fn run() -> Result {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>()?;
    let mut protocol = boot::open_protocol_exclusive::<GraphicsOutput>(handle)?;

    let mode = protocol
        .modes()
        .find(|m| m.info().resolution() == (800, 600))
        .ok_or(NyaStatus::_Debug(String::from(
            "800x600 GOP mode not found",
        )))?;
    protocol.set_mode(&mode)?;

    let (w, h) = protocol.current_mode_info().resolution();
    let mut canvas = Canvas::new(w as usize, h as usize);

    Text::new(
        "ｱｲｳｴｵ, ASCII_JP
    1x   0   1   2   3   4   5   6   7   8   9   A   B   C   D   E   F
    2x  SP   !  \"   #   $   %   &   '   (   )   *   +   ,   -   .   /
    3x   0   1   2   3   4   5   6   7   8   9   :   ;   <   =   >   ?
    4x   @   A   B   C   D   E   F   G   H   I   J   K   L   M   N   O
    5x   P   Q   R   S   T   U   V   W   X   Y   Z   [   ¥   ]   ^   _
    6x   `   a   b   c   d   e   f   g   h   i   j   k   l   m   n   o
    7x   p   q   r   s   t   u   v   w   x   y   z   {   |   }   ‾   DEL
    Ax       ｡   ｢   ｣   ､   ･   ｦ   ｧ   ｨ   ｩ   ｪ   ｫ   ｬ   ｭ   ｮ   ｯ
    Bx   ｰ   ｱ   ｲ   ｳ   ｴ   ｵ   ｶ   ｷ   ｸ   ｹ   ｺ   ｻ   ｼ   ｽ   ｾ   ｿ
    Cx   ﾀ   ﾁ   ﾂ   ﾃ   ﾄ   ﾅ   ﾆ   ﾇ   ﾈ   ﾉ   ﾊ   ﾋ   ﾌ   ﾍ   ﾎ   ﾏ
    Dx   ﾐ   ﾑ   ﾒ   ﾓ   ﾔ   ﾕ   ﾖ   ﾗ   ﾘ   ﾙ   ﾚ   ﾛ   ﾜ   ﾝ   ﾞ   ﾟ  ",
        Point::new(0, 15),
        MonoTextStyle::new(&FONT_10X20, Rgb888::WHITE),
    )
    .draw(&mut canvas)
    .ok();

    Text::new(
        "何意味",
        Point::new(5, 30),
        U8g2TextStyle::new(u8g2_font_boutique_bitmap_7x7_t_chinese3, Rgb888::WHITE),
    )
    .draw(&mut canvas)
    .ok();

    protocol.blt(BltOp::BufferToVideo {
        buffer: &canvas.buf,
        src: BltRegion::Full,
        dest: (0, 0),
        dims: (w as usize, h as usize),
    })?;

    Ok(())
}
