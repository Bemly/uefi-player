//! 统一控制面上传侧：`single` 同旧环走 `render.blt`（调用方配 `blt`，本层按生效路径分发，
//! 矩阵 `fps=max` soak 对齐）；多缓冲环（`tune.buffers` 影子环 + 解码窗环，见 `decoder::lazy`）
//! 先画影子再整推（撕裂压成一次 push）；AP 永不碰 GOP/Blt（`mp`/`exec` 纪律，Blt 整推只 BSP）。

use crate::config::{self, BltOpKind, RenderMode};
use crate::error::Result;
use crate::video::ascii_font::FONT_8X16;
use alloc::vec::Vec;
use uefi::boot::{ScopedProtocol, get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};

pub mod calibrate;
pub mod mp_copy;

pub(crate) use self::mp_copy::{calibrate_tps, copy_block, dynamic_row_bounds, tsc, u64_fast_copy};

pub struct Screen {
    gop: ScopedProtocol<GraphicsOutput>,
    stdout: usize,
    /// 后缓冲影子环（`tune.buffers > 0` 时用）：每槽一帧 `BltPixel`，播放/壁纸
    /// 先画影子再整推前台。校准/菜单文本不走这里（直写，零回归风险）。
    sh_ring: Vec<Vec<BltPixel>>,
    sh_w: usize,
    sh_h: usize,
    sh_k: usize,
}

impl Screen {
    pub fn new() -> Result<Self> {
        let handle = get_handle_for_protocol::<GraphicsOutput>()?;
        let mut gop = open_protocol_exclusive::<GraphicsOutput>(handle)?;
        // 首选 1080p：视频就是按 1920x1080 准备的，且大窗口更易读。
        // 固件没有该模式时保持默认（尽力而为，不报错）。
        let mode_1080p = gop.modes().find(|m| m.info().resolution() == (1920, 1080));
        if let Some(mode) = mode_1080p {
            if let Err(e) = gop.set_mode(&mode) {
                log::warn!("set 1080p GOP mode failed: {:?}", e);
            }
        }
        Ok(Self {
            gop,
            stdout: 0,
            sh_ring: Vec::new(),
            sh_w: 0,
            sh_h: 0,
            sh_k: 0,
        })
    }

    /// 独占 GOP 的 demo 跑完后重建：保持当前模式、不切分辨率，
    /// 保住 demo 画面（`new()` 会尝试切 1080p，不适合这里）。
    /// 影子环不继承（按需重建，尺寸按新模式来）。
    pub fn reacquire() -> Result<Self> {
        let handle = get_handle_for_protocol::<GraphicsOutput>()?;
        let gop = open_protocol_exclusive::<GraphicsOutput>(handle)?;
        Ok(Self {
            gop,
            stdout: 0,
            sh_ring: Vec::new(),
            sh_w: 0,
            sh_h: 0,
            sh_k: 0,
        })
    }

    pub fn get_gop(&mut self) -> &mut ScopedProtocol<GraphicsOutput> {
        &mut self.gop
    }

    /// 把文本光标复位到左上角（配合 `clear()` 在菜单/报错切屏时用）。
    pub fn reset_cursor(&mut self) {
        self.stdout = 0;
    }

    /// 把文本光标移到屏幕底部（独占 GOP 的 demo 跑完后用：
    /// 画面要留给用户看，不能 `clear()`，提示行打在底部避免盖住内容）。
    pub fn cursor_to_bottom(&mut self) {
        let (_, height) = self.gop.current_mode_info().resolution();
        self.stdout = height.saturating_sub(40);
    }

    pub fn draw_image(&mut self, width: u32, height: u32, pixels: &[BltPixel]) -> Result {
        // 我不知道为什么封装成这样了，但是它能工作！
        // 默认blt输出uefi::result::Result，这里?拆包然后Ok封装为crate::error::Result
        // ?外的Ok并不会影响错误抛出
        Ok(self.gop.blt(BltOp::BufferToVideo {
            buffer: pixels,
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (width as usize, height as usize),
        })?)
    }

    pub fn clear(&mut self) -> Result {
        let info = self.gop.current_mode_info();
        let (width, height) = info.resolution();

        // 使用 VideoFill 操作，这比传输像素数组快得多
        Ok(self.gop.blt(BltOp::VideoFill {
            // 黑色像素：Red=0, Green=0, Blue=0
            color: BltPixel::new(0, 0, 0),
            dest: (0, 0),
            dims: (width, height),
        })?)
    }

    // 糟糕的 ASCII 输出实现,一个字符的一个笔画开始渲染
    pub fn draw_str(&mut self, text: &str) {
        let mut x = 0;
        // 获取当前屏幕的宽度，用于自动换行
        let (width, height) = self.gop.current_mode_info().resolution();

        let fg = BltPixel::new(255, 255, 255);
        let bg = BltPixel::new(0, 0, 0);

        // 防止超出高度
        if self.stdout + 20 >= height {
            self.stdout = 0
        }

        for c in text.chars() {
            // --- 1. 处理换行逻辑 ---

            // 显式换行符处理
            if c == '\n' {
                x = 0;
                self.stdout += 18; // 换行高度（点阵16像素 + 2像素间距）
                if self.stdout >= height {
                    self.stdout = 0
                }
                continue;
            }

            // 超出屏幕宽度自动换行 (8 是字符宽度)
            if x + 8 > width {
                x = 0;
                self.stdout += 18;
                if self.stdout >= height {
                    self.stdout = 0
                }
            }

            // --- 2. 绘制字符 ---

            let index = (c as usize) & 0x7F;
            let glyph = &FONT_8X16[index];

            for row in 0..16 {
                let row_bits = glyph[row];
                for col in 0..8 {
                    let is_fg = (row_bits >> (7 - col)) & 1 == 1;
                    let color = if is_fg { fg } else { bg };

                    // 绘制像素
                    let _ = self.gop.blt(BltOp::VideoFill {
                        color,
                        dest: (x + col, self.stdout + row),
                        dims: (1, 1),
                    });
                }
            }

            // 字符绘制完后，坐标右移
            x += 8;
        }

        // 整个字符串画完后，最后额外换一行，防止下一次打印重叠
        self.stdout += 18;
    }

    // ---------- 渲染路径矩阵（配置文件驱动） ----------

    /// 统一上屏入口：按 `\player.toml` 的 `render.mode` 走 CPU 直写或 GOP Blt。
    /// 直写路 + `tune.buffers > 0` 时走影子缓冲（先画影子再整推，撕裂窗口压成
    /// 一次 push）；Blt 已是单引擎 push，本项无意义，直接走引擎。
    pub fn upload_frame(&mut self, width: usize, height: usize, pixels: &[BltPixel]) -> Result {
        match config::effective_render() {
            RenderMode::Blt => self.upload_blt(width, height, pixels),
            _ => {
                if config::tune_buffers() > 0 && self.ensure_shadow(width, height) {
                    self.upload_direct_shadowed(width, height, pixels)
                } else {
                    self.upload_direct(width, height, pixels)
                }
            }
        }
    }

    /// 影子环整备：按当前 `tune.buffers` 配 `w×h` 槽。尺寸/数量对不上就重建；
    /// 分配失败返回 false（调用方退回直写，只记 warning，不断流程）。
    /// `extend` 前已 `try_reserve_exact`，中途不再分配，不会 panic。
    fn ensure_shadow(&mut self, w: usize, h: usize) -> bool {
        let n = config::tune_buffers().min(16) as usize;
        if n == 0 || w == 0 || h == 0 {
            return false;
        }
        if self.sh_ring.len() == n && self.sh_w == w && self.sh_h == h {
            return true;
        }
        let mut ring: Vec<Vec<BltPixel>> = Vec::new();
        if ring.try_reserve_exact(n).is_err() {
            log::warn!("shadow: ring alloc failed, fallback direct");
            return false;
        }
        for _ in 0..n {
            let mut slot: Vec<BltPixel> = Vec::new();
            if slot.try_reserve_exact(w.saturating_mul(h)).is_err() {
                log::warn!("shadow: slot alloc failed, fallback direct");
                return false;
            }
            slot.extend(core::iter::repeat(BltPixel::new(0, 0, 0)).take(w * h));
            ring.push(slot);
        }
        self.sh_ring = ring;
        self.sh_w = w;
        self.sh_h = h;
        self.sh_k = 0;
        // 起用记一行（串口可断言；QEMU 黑盒矩阵靠它确认缓冲生效）。
        log::info!("shadow: {} bufs {}x{} armed", n, w, h);
        true
    }

    /// 用完即扔（单帧实测后）：后缓冲只在连续推送时有用，散调用完就放，
    /// 免得 `mp_draw` 另起环时双份常驻（峰值只留 N 份）。
    pub fn release_shadows(&mut self) {
        self.sh_ring = Vec::new();
        self.sh_w = 0;
        self.sh_h = 0;
        self.sh_k = 0;
    }

    /// 缓冲直写整帧：画进影子槽再整推前台。
    fn upload_direct_shadowed(
        &mut self,
        width: usize,
        height: usize,
        pixels: &[BltPixel],
    ) -> Result {
        let n = self.sh_ring.len();
        if n == 0 {
            return self.upload_direct(width, height, pixels);
        }
        let k = self.sh_k % n;
        self.sh_k = self.sh_k.wrapping_add(1);
        {
            let slot = &mut self.sh_ring[k];
            let len = width
                .saturating_mul(height)
                .min(pixels.len())
                .min(slot.len());
            slot[..len].copy_from_slice(&pixels[..len]);
        }
        self.push_shadow(k, 0, height)
    }

    /// 影子槽整推前台（`y0..y1` 行区，stride 感知逐行推）。调用方保证槽尺寸匹配。
    fn push_shadow(&mut self, k: usize, y0: usize, y1: usize) -> Result {
        let n = self.sh_ring.len();
        if n == 0 {
            return Ok(());
        }
        let stride = self.gop.current_mode_info().stride();
        let w = config::copy_width();
        let (sw, sh) = (self.sh_w, self.sh_h);
        let y0 = y0.min(sh);
        let y1 = y1.min(sh).max(y0);
        let mut fb = self.gop.frame_buffer();
        let dst = fb.as_mut_ptr();
        let src = self.sh_ring[k % n].as_ptr() as *const u8;
        for y in y0..y1 {
            unsafe { copy_block(dst.add(y * stride * 4), src.add(y * sw * 4), sw * 4, w) };
        }
        Ok(())
    }

    /// 走 GOP Blt 引擎（`BufferToVideo`）。注意：只有它是「内存帧 → 屏」的语义。
    fn upload_blt(&mut self, width: usize, height: usize, pixels: &[BltPixel]) -> Result {
        Ok(self.gop.blt(BltOp::BufferToVideo {
            buffer: pixels,
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (width, height),
        })?)
    }

    /// 所选 Blt 操作单发（引擎能力对照数）：B2V=整帧上屏；V2V=屏内对拷；
    /// Fill=纯色填充；V2B=回读到内存。只有 B2V 能上屏播视频，其余只出数不参选。
    fn blt_op_once(
        &mut self,
        op: BltOpKind,
        w: usize,
        h: usize,
        pixels: &mut [BltPixel],
    ) -> Result {
        match op {
            BltOpKind::BufferToVideo => self.upload_blt(w, h, pixels),
            BltOpKind::VideoToVideo => Ok(self.gop.blt(BltOp::VideoToVideo {
                src: (0, 0),
                dest: (0, 0),
                dims: (w, h),
            })?),
            BltOpKind::VideoFill => Ok(self.gop.blt(BltOp::VideoFill {
                color: BltPixel::new(0x11, 0x22, 0x33),
                dest: (0, 0),
                dims: (w, h),
            })?),
            BltOpKind::VideoToBltBuffer => Ok(self.gop.blt(BltOp::VideoToBltBuffer {
                buffer: pixels,
                src: (0, 0),
                dest: BltRegion::Full,
                dims: (w, h),
            })?),
        }
    }

    /// 行区上屏（壁纸菜单背景）：只画 rows `y0..height`，菜单文字区不动。
    /// Blt 走 `SubRectangle` 取源行区；direct 逐行拷（stride 感知）。
    /// direct + `tune.buffers > 0` 时画影子行区再整推同区。
    pub fn upload_frame_region(
        &mut self,
        width: usize,
        height: usize,
        y0: usize,
        pixels: &[BltPixel],
    ) -> Result {
        let y0 = y0.min(height);
        match config::effective_render() {
            RenderMode::Blt => Ok(self.gop.blt(BltOp::BufferToVideo {
                buffer: pixels,
                src: BltRegion::SubRectangle {
                    coords: (0, y0),
                    px_stride: width,
                },
                dest: (0, y0),
                dims: (width, height - y0),
            })?),
            _ => {
                if config::tune_buffers() > 0 && self.ensure_shadow(width, height) {
                    let n = self.sh_ring.len().max(1);
                    let k = self.sh_k % n;
                    self.sh_k = self.sh_k.wrapping_add(1);
                    {
                        let slot = &mut self.sh_ring[k];
                        for y in y0..height {
                            let o = y * width;
                            if o + width <= slot.len() && o + width <= pixels.len() {
                                slot[o..o + width].copy_from_slice(&pixels[o..o + width]);
                            }
                        }
                    }
                    return self.push_shadow(k, y0, height);
                }
                let stride = self.gop.current_mode_info().stride();
                let mut fb = self.gop.frame_buffer();
                let dst = fb.as_mut_ptr();
                let src = pixels.as_ptr() as *const u8;
                let w = config::copy_width();
                for y in y0..height {
                    copy_block(
                        unsafe { dst.add(y * stride * 4) },
                        unsafe { src.add(y * width * 4) },
                        width * 4,
                        w,
                    );
                }
                Ok(())
            }
        }
    }

    /// CPU 直写 framebuffer（stride 感知），按 P2 位宽选路。
    /// 位宽来自 `config::copy_width()`（`accel.level` × 硬件上限），调用方已门控，
    /// 这里只管执行（AVX 系跑完 `vzeroupper`，不污染后续 SSE）。
    fn upload_direct(&mut self, width: usize, height: usize, pixels: &[BltPixel]) -> Result {
        let stride = self.gop.current_mode_info().stride();
        let mut fb = self.gop.frame_buffer();
        let dst = fb.as_mut_ptr();
        let src = pixels.as_ptr() as *const u8;
        let w = config::copy_width();
        if stride == width {
            copy_block(dst, src, width * height * 4, w);
        } else {
            for y in 0..height {
                copy_block(
                    unsafe { dst.add(y * stride * 4) },
                    unsafe { src.add(y * width * 4) },
                    width * 4,
                    w,
                );
            }
        }
        Ok(())
    }
}
