//! 统一控制面上传侧：`single` 同旧环走 `render.blt`（调用方配 `blt`，本层按生效路径分发，
//! 矩阵 `fps=max` soak 对齐）；多缓冲环（`tune.buffers` 影子环 + 解码窗环，见 `decoder::lazy`）
//! 先画影子再整推（撕裂压成一次 push）；AP 永不碰 GOP/Blt（`mp`/`exec` 纪律，Blt 整推只 BSP）。
//!
//! `[display]` presentation-time 适配（见 `present`）：stretch 拉满全屏推；
//! fit 以 bar 色铺底（Blt 路 `VideoFill`，直写/影子路见各函数）+ 居中/锚定矩形；
//! native 1:1 + align 锚定（v2 默认居中，v1 左上，见 `present` 头）。
//! H.263 letterbox 系 encode-time 已定几何（`Clip` 内，kept），与本层互补不冗余。`upload_frame_region`（壁纸行区动画）
//! 与 `mp_*`/`dynamic` 多核路 v1 不变（原因见各函数头）。

// v2（`[display]` fit 五档 + align 五档）：几何全由 `present::geometry`
// （`scale::rect`）定，上传侧按 `place_axis` 双轴窗推（Blt 源窗/目标窗、
// 直写/影子行列双限），不写屏外、不读源外；native 默认居中（v1 左上，
// 行为变更见 `present` 头）；H.263 encode-time letterbox 保持不动。

use crate::config::{self, BltOpKind, DisplayFit, RenderMode};
use crate::error::Result;
use crate::video::ascii_font::FONT_8X16;
use alloc::vec::Vec;
use uefi::boot::{ScopedProtocol, get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};

pub mod calibrate;
pub mod mp_copy;
pub mod present;
pub mod scale;

pub(crate) use self::mp_copy::{calibrate_tps, copy_block, dynamic_row_bounds, tsc};

pub struct Screen {
    gop: ScopedProtocol<GraphicsOutput>,
    stdout: usize,
    /// 后缓冲影子环（`tune.buffers > 0` 时用）：每槽一帧 `BltPixel`，播放/壁纸
    /// 先画影子再整推前台。校准/菜单文本不走这里（直写，零回归风险）。
    sh_ring: Vec<Vec<BltPixel>>,
    sh_w: usize,
    sh_h: usize,
    sh_k: usize,
    /// `[display]` 缩放复用暂存（stretch 全屏 / fit 矩形，屏几何上限；失败回落
    /// native，不断流程）。壁纸/校准 1:1 快捷道不经此（零额外分配）。
    present_buf: Vec<BltPixel>,
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
            present_buf: Vec::new(),
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
            present_buf: Vec::new(),
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

    // ---------- 渲染路径矩阵（配置文件驱动） + `[display]` 适配（v2） ----------

    /// 统一上屏入口：按 `player.toml` 的 `render.mode` 走 CPU 直写或 GOP Blt。
    /// 直写路 + `tune.buffers > 0` 时走影子缓冲（先画影子再整推，撕裂窗口压成
    /// 一次 push）；Blt 已是单引擎 push，本项无意义，直接走引擎。
    ///
    /// `[display]` v2（`width/height` 为 clip 源几何 `sw×sh`，调用方传 `Clip::w/h`）：
    /// 几何全由 `present::geometry`（`scale::rect`：fit 五档 + align 五档）定；
    /// clip==屏时五档恒等，直走 1:1 快捷道（零缩放分配，校准矩阵计时零扰动）。
    /// H.263 letterbox 系 encode-time 已定几何（`Clip` 内，kept），与本层互补不冗余。
    /// `upload_frame_region`（壁纸行区动画）与 `mp_*`/`dynamic` 多核路 v1 不变
    /// （原因见各函数头）。
    pub fn upload_frame(&mut self, width: usize, height: usize, pixels: &[BltPixel]) -> Result {
        let (dw, dh) = self.gop.current_mode_info().resolution();
        if width == dw && height == dh {
            // clip==屏时五档恒等（align 无处锚），直走 1:1（零缩放分配）。
            return self.upload_present_native(width, height, pixels, dw, dh);
        }
        match present::fit() {
            DisplayFit::Native => self.upload_present_native(width, height, pixels, dw, dh),
            _ => self.upload_present_scaled(width, height, pixels, dw, dh),
        }
    }

    /// 1:1 路（native；scale_down-放得下经 scaled 委派至此）：源窗直取，零缩放分配。
    /// 窗由 `place_axis` 双轴定（align 锚定；oversize 轴裁剪窗亦按 align；
    /// 默认 center 即 v2 居中，v1 左上的行为变更见 `present` 头）。
    /// 行列双限：不写屏外、不读源外（v1 直写 `0..h` 全行是 oversize OOB 的根因）。
    fn upload_present_native(
        &mut self,
        sw: usize,
        sh: usize,
        pixels: &[BltPixel],
        dw: usize,
        dh: usize,
    ) -> Result {
        use super::graphics::scale::{align_sides, place_axis};
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return Ok(());
        }
        if pixels.len() < sw.saturating_mul(sh) {
            return Ok(());
        }
        let (lo_x, hi_x, lo_y, hi_y) = align_sides(present::align());
        let (dx, sx, vw) = place_axis(sw as u32, dw as u32, lo_x, hi_x);
        let (dy, sy, vh) = place_axis(sh as u32, dh as u32, lo_y, hi_y);
        let (dx, sx, vw) = (dx as usize, sx as usize, vw as usize);
        let (dy, sy, vh) = (dy as usize, sy as usize, vh as usize);
        if vw == 0 || vh == 0 {
            return Ok(());
        }
        let fill = vw < dw || vh < dh;
        match config::effective_render() {
            RenderMode::Blt => {
                if fill {
                    self.gop.blt(BltOp::VideoFill {
                        color: present::bar_pixel(),
                        dest: (0, 0),
                        dims: (dw, dh),
                    })?;
                }
                Ok(self.gop.blt(BltOp::BufferToVideo {
                    buffer: pixels,
                    src: BltRegion::SubRectangle {
                        coords: (sx, sy),
                        px_stride: sw,
                    },
                    dest: (dx, dy),
                    dims: (vw, vh),
                })?)
            }
            _ => {
                if config::tune_buffers() > 0
                    && self.ensure_shadow(dw, dh)
                    && !self.sh_ring.is_empty()
                {
                    // 屏尺寸槽：按需 bar 底 + 源窗拷目标窗 + 整推（v1 小槽只推左上，
                    // 屏其余部分残留旧帧；v2 恒屏槽，居中/锚定与直写一致）。
                    let nslots = self.sh_ring.len();
                    let k = self.sh_k % nslots;
                    self.sh_k = self.sh_k.wrapping_add(1);
                    {
                        let slot = &mut self.sh_ring[k];
                        if fill {
                            let bar = present::bar_pixel();
                            for p in slot.iter_mut() {
                                *p = bar;
                            }
                        }
                        let dst = slot.as_mut_ptr() as *mut u8;
                        let src = pixels.as_ptr() as *const u8;
                        Self::blit_rows_direct(dst, dw, src, sw, dx, dy, sx, sy, vw, vh);
                    }
                    self.push_shadow(k, 0, dh)
                } else {
                    if fill {
                        self.gop.blt(BltOp::VideoFill {
                            color: present::bar_pixel(),
                            dest: (0, 0),
                            dims: (dw, dh),
                        })?;
                    }
                    let stride = self.gop.current_mode_info().stride();
                    if stride < dw {
                        return Ok(());
                    }
                    let mut fb = self.gop.frame_buffer();
                    let dst = fb.as_mut_ptr();
                    let src = pixels.as_ptr() as *const u8;
                    Self::blit_rows_direct(dst, stride, src, sw, dx, dy, sx, sy, vw, vh);
                    Ok(())
                }
            }
        }
    }

    /// 缩放路统一实现（stretch/fit/cover；scale_down-放不下亦经此，`geometry`
    /// 已按 fit 折好 `fw×fh`；放得下即恒等，委派 1:1 免暂存）。
    /// 暂存分配失败回落 1:1（不断流程；v1 同形）。
    fn upload_present_scaled(
        &mut self,
        sw: usize,
        sh: usize,
        pixels: &[BltPixel],
        dw: usize,
        dh: usize,
    ) -> Result {
        use super::graphics::scale::{align_sides, place_axis, scale_bilinear};
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return Ok(());
        }
        if pixels.len() < sw.saturating_mul(sh) {
            return Ok(());
        }
        let (_, _, fw, fh) = present::geometry(sw as u32, sh as u32, dw as u32, dh as u32);
        let (fw, fh) = (fw as usize, fh as usize);
        if fw == 0 || fh == 0 {
            return Ok(());
        }
        if fw == sw && fh == sh {
            // scale_down-小（及同窗 fit）：恒等变换，退 1:1 免暂存分配。
            return self.upload_present_native(sw, sh, pixels, dw, dh);
        }
        let (lo_x, hi_x, lo_y, hi_y) = align_sides(present::align());
        let (dx, sx, vw) = place_axis(fw as u32, dw as u32, lo_x, hi_x);
        let (dy, sy, vh) = place_axis(fh as u32, dh as u32, lo_y, hi_y);
        let (dx, sx, vw) = (dx as usize, sx as usize, vw as usize);
        let (dy, sy, vh) = (dy as usize, sy as usize, vh as usize);
        if vw == 0 || vh == 0 {
            return Ok(());
        }
        let n = fw.saturating_mul(fh);
        let scaled_ok = {
            let scratch = self.present_scratch(n);
            if scratch.len() < n {
                false
            } else {
                scale_bilinear(pixels, sw, sh, scratch, fw, fh, config::copy_width());
                true
            }
        };
        if !scaled_ok {
            return self.upload_present_native(sw, sh, pixels, dw, dh);
        }
        let fill = vw < dw || vh < dh;
        match config::effective_render() {
            RenderMode::Blt => {
                if fill {
                    self.gop.blt(BltOp::VideoFill {
                        color: present::bar_pixel(),
                        dest: (0, 0),
                        dims: (dw, dh),
                    })?;
                }
                let buf = &self.present_buf[..n.min(self.present_buf.len())];
                if buf.len() < n {
                    return self.upload_present_native(sw, sh, pixels, dw, dh);
                }
                Ok(self.gop.blt(BltOp::BufferToVideo {
                    buffer: buf,
                    src: BltRegion::SubRectangle {
                        coords: (sx, sy),
                        px_stride: fw,
                    },
                    dest: (dx, dy),
                    dims: (vw, vh),
                })?)
            }
            _ => {
                if config::tune_buffers() > 0
                    && self.ensure_shadow(dw, dh)
                    && !self.sh_ring.is_empty()
                {
                    // 屏尺寸槽：按需 bar 底 + 暂存窗拷目标窗 + 整推。
                    let nslots = self.sh_ring.len();
                    let k = self.sh_k % nslots;
                    self.sh_k = self.sh_k.wrapping_add(1);
                    {
                        let slot = &mut self.sh_ring[k];
                        if fill {
                            let bar = present::bar_pixel();
                            for p in slot.iter_mut() {
                                *p = bar;
                            }
                        }
                        let dst = slot.as_mut_ptr() as *mut u8;
                        let src = self.present_buf.as_ptr() as *const u8;
                        Self::blit_rows_direct(dst, dw, src, fw, dx, dy, sx, sy, vw, vh);
                    }
                    self.push_shadow(k, 0, dh)
                } else {
                    if fill {
                        self.gop.blt(BltOp::VideoFill {
                            color: present::bar_pixel(),
                            dest: (0, 0),
                            dims: (dw, dh),
                        })?;
                    }
                    let stride = self.gop.current_mode_info().stride();
                    if stride < dw {
                        return Ok(());
                    }
                    let mut fb = self.gop.frame_buffer();
                    let dst = fb.as_mut_ptr();
                    let src = self.present_buf.as_ptr() as *const u8;
                    Self::blit_rows_direct(dst, stride, src, fw, dx, dy, sx, sy, vw, vh);
                    Ok(())
                }
            }
        }
    }

    /// 窗行拷（关联函数，无 `self` 借用，Blt/直写/影子三路共用；调用方保证
    /// `dx+vw<=stride`、`sx+vw<=src_stride_px`、`dy+vh`/`sy+vh` 在各自高内——
    /// `place_axis` 窗恒成立，`stride<dw` 的畸形 GOP 调用方已提前跳过）。
    /// 整行连续时（`dx==sx==0` 且窗宽==两边步幅）单发整拷，否则逐行（行列双限）。
    #[allow(clippy::too_many_arguments)] // 窗参数 6 个 + 两端指针/步幅 4 个，拆包反而不直观（库内同形先例见 `calibrate.rs`）。
    fn blit_rows_direct(
        dst: *mut u8,
        stride: usize,
        src: *const u8,
        src_stride_px: usize,
        dx: usize,
        dy: usize,
        sx: usize,
        sy: usize,
        vw: usize,
        vh: usize,
    ) {
        let w = config::copy_width();
        if dx == 0 && sx == 0 && vw == stride && vw == src_stride_px {
            unsafe {
                copy_block(
                    dst.add(dy * stride * 4),
                    src.add(sy * src_stride_px * 4),
                    vh.saturating_mul(vw).saturating_mul(4),
                    w,
                );
            }
        } else {
            for y in 0..vh {
                unsafe {
                    copy_block(
                        dst.add((dy + y) * stride * 4 + dx * 4),
                        src.add((sy + y) * src_stride_px * 4 + sx * 4),
                        vw * 4,
                        w,
                    );
                }
            }
        }
    }

    /// present 复用暂存：保证 `len >= n`（`try_reserve` + 黑底补齐，不 panic；
    /// 失败时返回现有短切片，调用方回落 1:1，不断流程）。
    fn present_scratch(&mut self, n: usize) -> &mut [BltPixel] {
        if self.present_buf.len() < n {
            let add = n.saturating_sub(self.present_buf.len());
            if self.present_buf.try_reserve_exact(add).is_err() {
                let len = self.present_buf.len();
                return &mut self.present_buf[..len];
            }
            self.present_buf
                .extend(core::iter::repeat_n(BltPixel::new(0, 0, 0), add));
        }
        let len = self.present_buf.len();
        let end = n.min(len);
        &mut self.present_buf[..end]
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
            slot.extend(core::iter::repeat_n(BltPixel::new(0, 0, 0), w * h));
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

    /// 影子槽整推前台（`y0..y1` 行区，stride 感知逐行推）。调用方保证槽尺寸匹配。
    fn push_shadow(&mut self, k: usize, y0: usize, y1: usize) -> Result {
        let n = self.sh_ring.len();
        if n == 0 {
            return Ok(());
        }
        let stride = self.gop.current_mode_info().stride();
        let w = config::copy_width();
        let (sw, sh) = (self.sh_w, self.sh_h);
        // 槽行宽恒 ≤ 步幅（GOP stride 理论上 ≥ 屏宽；钳住防 OOB 写，行区/oversize 皆安全）。
        let row_px = sw.min(stride);
        let y0 = y0.min(sh);
        let y1 = y1.min(sh).max(y0);
        let mut fb = self.gop.frame_buffer();
        let dst = fb.as_mut_ptr();
        let src = self.sh_ring[k % n].as_ptr() as *const u8;
        for y in y0..y1 {
            unsafe { copy_block(dst.add(y * stride * 4), src.add(y * sw * 4), row_px * 4, w) };
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
    ///
    /// `[display]` v2 不变（壁纸恒屏尺寸源时恒等通过，零代价；行区再缩放会错位
    /// 文字区，故不行 `rect` 缩放）+ 窗裁加固：行区与屏相交（`clip_blit`），
    /// 屏小于壁纸时只推屏内窗，不写屏外、不读源外（`Blt dest dims` 不出屏，
    /// 直写/影子行列双限）。
    pub fn upload_frame_region(
        &mut self,
        width: usize,
        height: usize,
        y0: usize,
        pixels: &[BltPixel],
    ) -> Result {
        use super::graphics::scale::clip_blit;
        let y0 = y0.min(height);
        if width == 0 || height == 0 {
            return Ok(());
        }
        if pixels.len() < width.saturating_mul(height) {
            return Ok(());
        }
        let (dw, dh) = self.gop.current_mode_info().resolution();
        let Some((dx, dy, sx, sy, vw, vh)) = clip_blit(
            0,
            y0 as u32,
            width as u32,
            height.saturating_sub(y0) as u32,
            dw as u32,
            dh as u32,
        ) else {
            return Ok(());
        };
        let (dx, dy, sx, sy, vw, vh) = (
            dx as usize,
            dy as usize,
            sx as usize,
            sy as usize,
            vw as usize,
            vh as usize,
        );
        match config::effective_render() {
            RenderMode::Blt => Ok(self.gop.blt(BltOp::BufferToVideo {
                buffer: pixels,
                src: BltRegion::SubRectangle {
                    coords: (sx, sy),
                    px_stride: width,
                },
                dest: (dx, dy),
                dims: (vw, vh),
            })?),
            _ => {
                if config::tune_buffers() > 0 && self.ensure_shadow(width, height) {
                    let n = self.sh_ring.len().max(1);
                    let k = self.sh_k % n;
                    self.sh_k = self.sh_k.wrapping_add(1);
                    {
                        let slot = &mut self.sh_ring[k];
                        let dst = slot.as_mut_ptr() as *mut u8;
                        let src = pixels.as_ptr() as *const u8;
                        Self::blit_rows_direct(dst, width, src, width, dx, dy, sx, sy, vw, vh);
                    }
                    return self.push_shadow(k, dy, dy + vh);
                }
                let stride = self.gop.current_mode_info().stride();
                if stride < width {
                    return Ok(());
                }
                let mut fb = self.gop.frame_buffer();
                let dst = fb.as_mut_ptr();
                let src = pixels.as_ptr() as *const u8;
                Self::blit_rows_direct(dst, stride, src, width, dx, dy, sx, sy, vw, vh);
                Ok(())
            }
        }
    }
}
