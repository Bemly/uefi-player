use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use core::time::Duration;
use uefi::boot::{create_event, get_handle_for_protocol, open_protocol_exclusive, stall, EventType, ScopedProtocol, Tpl};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, Mode};
use uefi::proto::pi::mp::MpServices;
use crate::config::{self, CopyWidth, DistMode, RenderMode, VsyncMode, WorkersMode};
use crate::error::Result;
use crate::video::ascii_font::FONT_8X16;
use crate::video::decoder::VideoMemoryRaw;

pub struct Screen {
    gop: ScopedProtocol<GraphicsOutput>,
    stdout: usize,
}

impl Screen {
    pub fn new() -> Result<Self> {
        let handle = get_handle_for_protocol::<GraphicsOutput>()?;
        let mut gop = open_protocol_exclusive::<GraphicsOutput>(handle)?;
        // 首选 1080p：视频就是按 1920x1080 准备的，且大窗口更易读。
        // 固件没有该模式时保持默认（尽力而为，不报错）。
        let mode_1080p = gop
            .modes()
            .find(|m| m.info().resolution() == (1920, 1080));
        if let Some(mode) = mode_1080p {
            if let Err(e) = gop.set_mode(&mode) {
                log::warn!("set 1080p GOP mode failed: {:?}", e);
            }
        }
        Ok(Self { gop, stdout: 0 })
    }

    /// 独占 GOP 的 demo 跑完后重建：保持当前模式、不切分辨率，
    /// 保住 demo 画面（`new()` 会尝试切 1080p，不适合这里）。
    pub fn reacquire() -> Result<Self> {
        let handle = get_handle_for_protocol::<GraphicsOutput>()?;
        let gop = open_protocol_exclusive::<GraphicsOutput>(handle)?;
        Ok(Self { gop, stdout: 0 })
    }

    pub fn get_gop(&mut self) -> &mut ScopedProtocol<GraphicsOutput> { &mut self.gop }

    /// 把文本光标复位到左上角（配合 `clear()` 在菜单/报错切屏时用）。
    pub fn reset_cursor(&mut self) { self.stdout = 0; }

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
        if self.stdout + 20 >= height { self.stdout = 0 }

        for c in text.chars() {
            // --- 1. 处理换行逻辑 ---

            // 显式换行符处理
            if c == '\n' {
                x = 0;
                self.stdout += 18; // 换行高度（点阵16像素 + 2像素间距）
                if self.stdout >= height { self.stdout = 0 }
                continue;
            }

            // 超出屏幕宽度自动换行 (8 是字符宽度)
            if x + 8 > width {
                x = 0;
                self.stdout += 18;
                if self.stdout >= height { self.stdout = 0 }
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

    pub fn draw_all_mem_raw_zero_copy(&mut self, video: &mut VideoMemoryRaw, width: usize, height: usize) {
        // 获取下一帧的原始像素引用
        if let Some(pixel_slice) = video.next_frame() {
            // 直接绘制到屏幕
            // 假设你的屏幕分辨率和视频一致，从 (0,0) 开始画
            self.gop.blt(
                BltOp::BufferToVideo {
                    buffer: pixel_slice,
                    src: BltRegion::Full,
                    dest: (0, 0),
                    dims: (width, height),
                }
            ).ok();
        } else {
            video.rewind();
        }
    }


    /// 高性能显存直接写入
    pub fn draw_fast_direct_copy(
        &mut self,
        video: &mut VideoMemoryRaw,
        width: usize,
        height: usize
    ) {
        // 1. 获取下一帧
        let pixel_slice = match video.next_frame() {
            Some(slice) => slice,
            None => {
                video.rewind();
                return;
            }
        };

        // 2. 先提取 ModeInfo（此时 gop 会被借用，但在这一行结束后就会释放）
        let mode_info = self.gop.current_mode_info();
        let stride = mode_info.stride();

        // 3. 再获取 FrameBuffer（此时 gop 被独占借用）
        let mut fb = self.gop.frame_buffer();
        let dest_ptr = fb.as_mut_ptr();

        // 4. 执行内存拷贝
        unsafe {
            if stride == width {
                // 全局一次性拷贝
                core::ptr::copy_nonoverlapping(
                    pixel_slice.as_ptr() as *const u8,
                    dest_ptr,
                    width * height * 4
                );
            } else {
                // 考虑 Stride 的逐行拷贝
                let src_ptr = pixel_slice.as_ptr() as *const u8;
                for y in 0..height {
                    let row_src = src_ptr.add(y * width * 4);
                    let row_dest = dest_ptr.add(y * stride * 4);
                    core::ptr::copy_nonoverlapping(row_src, row_dest, width * 4);
                }
            }
        }
    }


    #[inline]
    pub fn draw_u64_optimized(
        &mut self,
        video: &mut VideoMemoryRaw,
        width: usize,
        height: usize,
        stride: usize,
        is_continuous: bool,
        dest_ptr: *mut u8
    ) {
        let pixel_slice = match video.next_frame() {
            Some(slice) => slice,
            None => { video.rewind(); return; }
        };

        let src_ptr = pixel_slice.as_ptr() as *const u8;

        unsafe {
            if is_continuous {
                // 全屏连续拷贝
                let total_bytes = width * height * 4;
                u64_fast_copy(src_ptr, dest_ptr, total_bytes);
            } else {
                // 按行拷贝（处理 Stride）
                let row_bytes = width * 4;
                let stride_bytes = stride * 4;
                for y in 0..height {
                    let row_src = src_ptr.add(y * row_bytes);
                    let row_dest = dest_ptr.add(y * stride_bytes);
                    u64_fast_copy(row_src, row_dest, row_bytes);
                }
            }
        }
    }

    pub fn draw_u64_optimized_loop(&mut self, video: &mut VideoMemoryRaw, width: usize, height: usize) {
        let mode_info = self.gop.current_mode_info();
        let stride = mode_info.stride();
        let mut fb = self.gop.frame_buffer();
        let is_continuous = stride == width;
        let dest_ptr = fb.as_mut_ptr();

        loop {
            self.draw_u64_optimized(video, width, height, stride, is_continuous, dest_ptr);
        }
    }

    pub fn parallel_video_draw(&mut self, video: &mut VideoMemoryRaw, width: usize, height: usize) -> Result {
        let mp_handle = get_handle_for_protocol::<MpServices>()?;
        let mp_services = open_protocol_exclusive::<MpServices>(mp_handle)?;

        // 1. 识别所有可用的 AP（排除主核 BSP）
        let proc_count = mp_services.get_number_of_processors()?;

        Ok(())
    }

    // ---------- 渲染路径矩阵（配置文件驱动） ----------

    /// 统一上屏入口：按 `\player.toml` 的 `render.mode` 走 CPU 直写或 GOP Blt。
    pub fn upload_frame(&mut self, width: usize, height: usize, pixels: &[BltPixel]) -> Result {
        match config::effective_render() {
            RenderMode::Blt => self.upload_blt(width, height, pixels),
            _ => self.upload_direct(width, height, pixels),
        }
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

    /// 行区上屏（壁纸菜单背景）：只画 rows `y0..height`，菜单文字区不动。
    /// Blt 走 `SubRectangle` 取源行区；direct 逐行拷（stride 感知）。
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

    /// `render.mode/workers/dist/vsync = "auto"` 的开机矩阵校准（`\player.toml` 驱动）。
    ///
    /// - 首启（任一 auto 无记忆，见 `config::needs_render_bench`）必跑全矩阵：
    ///   直写 vs Blt 取快；直写路再比 BSP 单核 vs 全核并行，全核路再比
    ///   静态分区 vs tile 自调度 vs dynamic 加权（三选一）；冠军路按 60fps 预算跑一遍，
    ///   零超期则 vsync 开否则关。
    /// - 全核 cell 用 AP 常驻循环 + 轮次同步（`gop_bench` 方法论：单发 startup
    ///   测到的是 SIPI 唤醒开销而非扩展性，必须常驻 + 热身吸收后再计时）。
    /// - 同组变体轮转交替计时（`gop_simd` 方法论：杀时间漂移），结论写回
    ///   `measured_*`（`None` 维度不动盘），下次启动有记忆直接复用快启。
    /// - 会短暂刷花屏（随后菜单/播放会重画）。
    pub fn ensure_render_calibrated(&mut self) {
        if !config::needs_render_bench() {
            config::apply_measured_render();
            log::info!(
                "render auto: reuse measured render={:?} workers={:?} dist={:?} vsync={:?} cores={}",
                config::measured_render(),
                config::measured_workers(),
                config::measured_dist(),
                config::measured_vsync(),
                config::measured_cores(),
            );
            return;
        }
        // 记忆先装进本启动结论，校准只覆盖本次实测的维度。
        config::apply_measured_render();

        let frames = config::tune_frames().max(1) as u64;
        let tps = calibrate_tps();

        let (w, h) = self.gop.current_mode_info().resolution();
        let (w, h) = (w as usize, h as usize);
        let pixels = vec![BltPixel::new(0x11, 0x22, 0x33); w * h];

        // ---- 待定维度：显式值直接采用，只测 auto 且无记忆的 ----
        let probe_render = config::render_mode() == RenderMode::Auto
            && config::measured_render() == RenderMode::Auto;
        let fixed_render = if probe_render {
            None
        } else if config::render_mode() == RenderMode::Auto {
            Some(config::measured_render())
        } else {
            Some(config::render_mode())
        };
        // workers 只在直写路径有意义（Blt 恒单线程）：渲染定死 Blt 则不测。
        let direct_in_play = fixed_render
            .map(|m| m == RenderMode::Direct)
            .unwrap_or(true);
        let probe_workers = direct_in_play
            && config::workers_mode() == WorkersMode::Auto
            && config::measured_workers() == WorkersMode::Auto;
        let fixed_workers = if probe_workers {
            None
        } else if config::workers_mode() == WorkersMode::Auto {
            Some(config::measured_workers())
        } else {
            Some(config::workers_mode())
        };
        let probe_vsync =
            config::vsync_mode() == VsyncMode::Auto && config::measured_vsync() == VsyncMode::Auto;
        // dist 只在“直写 + 全核可能”时有意义（Blt/单核下忽略，免空转）。
        let mp_all_in_play =
            direct_in_play && (probe_workers || fixed_workers == Some(WorkersMode::All));
        let probe_dist = mp_all_in_play
            && config::dist_mode() == DistMode::Auto
            && config::measured_dist() == DistMode::Auto;
        let fixed_dist = if probe_dist {
            None
        } else if config::dist_mode() == DistMode::Auto {
            Some(config::measured_dist())
        } else {
            Some(config::dist_mode())
        };

        // ---- MP 会话（全核 cell / 全核冠军的 vsync 探测共用，用完即退） ----
        let mut mp_store = None;
        if let Ok(h) = get_handle_for_protocol::<MpServices>() {
            if let Ok(m) = open_protocol_exclusive::<MpServices>(h) {
                mp_store = Some(m);
            }
        }
        let n_cores = mp_store
            .as_deref()
            .and_then(|m| m.get_number_of_processors().ok())
            .map(|c| c.enabled.max(1))
            .unwrap_or(1);
        let want_mp = direct_in_play && (probe_workers || fixed_workers == Some(WorkersMode::All));
        // AP 常驻会话（栈上 ctx + 轮次号驱动，方法论见 gop_bench 计时轮）。
        let cal_round = AtomicUsize::new(0);
        let cal_done = AtomicUsize::new(0);
        // tile 游标 + 模式开关 + dynamic 计时/边界（三 cell 共用一个会话，轮次间翻转）。
        let cal_mode = AtomicU8::new(0);
        let cal_tile = AtomicUsize::new(0);
        // dynamic 定权状态：各核上一轮静态 copy 耗时（AP 自记，BSP 读来算权重），
        // 加权行边界（n+1 项，BSP 写、mode/round 发布后 AP 读）。Box 漏存，会话期常驻。
        let cal_times: &mut [AtomicUsize] = Box::leak(
            (0..n_cores.max(1))
                .map(|_| AtomicUsize::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let cal_bounds: &mut [AtomicUsize] = Box::leak(
            dynamic_row_bounds(&vec![0u64; n_cores.max(1)], n_cores.max(1), 0, h)
                .into_iter()
                .map(AtomicUsize::new)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let mut mp_drive: Option<MpDrive> = None;
        if want_mp {
            if let Some(mp) = mp_store.as_deref() {
                let fb_base = self.gop.frame_buffer().as_mut_ptr();
                let fb_stride = self.gop.current_mode_info().stride() * 4;
                let ctx = MpCalCtx {
                    mp,
                    fb_base,
                    fb_stride,
                    src: pixels.as_ptr() as *const u8,
                    row_bytes: w * 4,
                    height: h,
                    n: n_cores,
                    round: &cal_round,
                    done: &cal_done,
                    mode: &cal_mode,
                    tile: &cal_tile,
                    tile_rows: config::tune_tile_rows() as usize,
                    tiles_total: h.div_ceil(config::tune_tile_rows().max(1) as usize),
                    times: cal_times.as_ptr(),
                    bounds: cal_bounds.as_ptr(),
                };
                // ctx 必须活到 AP 退场：Box 漏存，函数尾 QUIT + stall 后不再访问。
                let arg_ptr = Box::leak(Box::new(ctx)) as *mut _ as *mut c_void;
                match unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None) } {
                    Ok(ev) => {
                        // Event 无 Drop，移进启动调用后固件侧保持有效（同 video.rs）。
                        if mp
                            .startup_all_aps(false, ap_cal_copy, arg_ptr, Some(ev), None)
                            .is_ok()
                        {
                        let bsp_id = mp.who_am_i().unwrap_or(0) % n_cores.max(1);
                        let (by0, by1) = rows_for_cal(bsp_id, n_cores, h);
                        mp_drive = Some(MpDrive {
                            n: n_cores,
                            bsp_id,
                            fb_base,
                            fb_stride,
                            src: pixels.as_ptr() as *const u8,
                            row_bytes: w * 4,
                            height: h,
                            bsp_y0: by0,
                            bsp_y1: by1,
                            expected: n_cores.saturating_sub(1),
                            round: &cal_round,
                            done: &cal_done,
                    mode: &cal_mode,
                    tile: &cal_tile,
                    tile_rows: config::tune_tile_rows() as usize,
                    tiles_total: h.div_ceil(config::tune_tile_rows().max(1) as usize),
                            times: cal_times.as_ptr(),
                            bounds: cal_bounds.as_ptr(),
                            next: 0,
                            tps,
                        });
                        } else {
                            log::warn!("render auto: MP startup failed, all-core cell SKIP");
                        }
                    }
                    Err(e) => {
                        log::warn!("render auto: MP event failed: {:?}, all-core cell SKIP", e)
                    }
                }
            }
        }

        // 各热身一次（单核路；多核路热身在会话计时里做）。
        let _ = self.upload_direct(w, h, &pixels);
        let _ = self.upload_blt(w, h, &pixels);

        // ---- 轮转交替计时：每轮各跑一次，累加 ticks ----
        let time_direct =
            direct_in_play && (probe_render || fixed_render == Some(RenderMode::Direct));
        let time_blt = fixed_render.map(|m| m == RenderMode::Blt).unwrap_or(true);
        let time_mp =
            mp_drive.is_some() && (probe_workers || fixed_workers == Some(WorkersMode::All));
        // static/tile/dynamic 三 cell：只测需要的（auto 全测，显式只测选中的）。
        let time_static = time_mp && (probe_dist || fixed_dist == Some(DistMode::Static));
        let time_tile = time_mp && (probe_dist || fixed_dist == Some(DistMode::Tile));
        let mut time_dynamic = time_mp && (probe_dist || fixed_dist == Some(DistMode::Dynamic));
        let mut acc_direct = 0u64;
        let mut acc_static = 0u64;
        let mut acc_tile = 0u64;
        let mut acc_dynamic = 0u64;
        let mut acc_blt = 0u64;
        // 多核热身各 1 轮（吸收 AP 唤醒延迟，不计时）。
        if time_static {
            mp_drive.as_mut().expect("mp drive").warmup_static();
        }
        if time_tile {
            mp_drive.as_mut().expect("mp drive").warmup_tiled();
        }
        // dynamic 定权：先跑一轮静态给各核计时（AP 在静态分支自记耗时），
        // BSP 读耗时算加权边界，再热身一轮 dynamic。probe 失败说明 MP 已坏，
        // 与静态/tile 熔断同处理：整个 MP 会话作废退回单核。
        if time_dynamic {
            let ok = mp_drive
                .as_mut()
                .map(|d| d.probe_dynamic())
                .unwrap_or(false);
            if ok {
                mp_drive.as_mut().expect("mp drive").warmup_dynamic();
            } else {
                acc_static = 0;
                acc_tile = 0;
                acc_dynamic = 0;
                mp_drive = None;
                time_dynamic = false;
            }
        }
        for _ in 0..frames {
            if time_direct {
                let t = tsc();
                let _ = self.upload_direct(w, h, &pixels);
                acc_direct += tsc().saturating_sub(t);
            }
            if time_static {
                let t = tsc();
                let ok = mp_drive.as_mut().expect("mp drive").run_round();
                let dt = tsc().saturating_sub(t);
                if ok {
                    acc_static += dt;
                } else {
                    // 熔断：本轮 AP 没跟上，会话作废，三个 MP cell 全退回单核。
                    acc_static = 0;
                    acc_tile = 0;
                    acc_dynamic = 0;
                    mp_drive = None;
                    break;
                }
            }
            if time_tile {
                let Some(drive) = mp_drive.as_mut() else {
                    break;
                };
                let t = tsc();
                let ok = drive.run_round_tiled();
                let dt = tsc().saturating_sub(t);
                if ok {
                    acc_tile += dt;
                } else {
                    // 熔断：本轮 AP 没跟上，会话作废，三个 MP cell 全退回单核。
                    acc_static = 0;
                    acc_tile = 0;
                    acc_dynamic = 0;
                    mp_drive = None;
                    break;
                }
            }
            if time_dynamic {
                let Some(drive) = mp_drive.as_mut() else {
                    break;
                };
                let t = tsc();
                let ok = drive.run_round_dynamic();
                let dt = tsc().saturating_sub(t);
                if ok {
                    acc_dynamic += dt;
                } else {
                    // 熔断：同上，三个 MP cell 全退回单核。
                    acc_static = 0;
                    acc_tile = 0;
                    acc_dynamic = 0;
                    mp_drive = None;
                    break;
                }
            }
            if time_blt {
                let t = tsc();
                let _ = self.upload_blt(w, h, &pixels);
                acc_blt += tsc().saturating_sub(t);
            }
        }
        let mp_alive = mp_drive.is_some();
        let time_static = time_static && mp_alive;
        let time_tile = time_tile && mp_alive;
        let time_dynamic = time_dynamic && mp_alive;

        // ---- 决胜 ----
        // 直写冠军：多核 cell 跳过/失败则退回单核。
        let static_valid = time_static && acc_static > 0;
        let tile_valid = time_tile && acc_tile > 0;
        let dynamic_valid = time_dynamic && acc_dynamic > 0;
        let mut direct_ticks = if time_direct { acc_direct } else { u64::MAX };
        if static_valid {
            direct_ticks = direct_ticks.min(acc_static);
        }
        if tile_valid {
            direct_ticks = direct_ticks.min(acc_tile);
        }
        if dynamic_valid {
            direct_ticks = direct_ticks.min(acc_dynamic);
        }
        // 只在“计时且有效”的 cell 里取最小（未测的 acc 为 0，不能参与 min）。
        let mut mp_best_ticks = u64::MAX;
        if static_valid {
            mp_best_ticks = mp_best_ticks.min(acc_static.max(1));
        }
        if tile_valid {
            mp_best_ticks = mp_best_ticks.min(acc_tile.max(1));
        }
        if dynamic_valid {
            mp_best_ticks = mp_best_ticks.min(acc_dynamic.max(1));
        }
        let mp_best = mp_best_ticks != u64::MAX;
        let winner_workers = if !direct_in_play {
            WorkersMode::BspOnly // Blt 路径恒单线程，workers 无意义
        } else if probe_workers {
            if mp_best && mp_best_ticks < acc_direct.max(1) {
                WorkersMode::All
            } else {
                WorkersMode::BspOnly
            }
        } else {
            fixed_workers.unwrap_or(WorkersMode::BspOnly)
        };
        let winner_render = if probe_render {
            if time_blt && acc_blt < direct_ticks {
                RenderMode::Blt
            } else {
                RenderMode::Direct
            }
        } else {
            fixed_render.unwrap_or(RenderMode::Direct)
        };
        // 多核分工：Blt/single 下无意义不记；显式值直接采用。
        // auto 时三个有效 cell 取计时最小（全无效退回 tile 无害默认）。
        let winner_dist =
            if winner_render != RenderMode::Direct || winner_workers != WorkersMode::All {
                DistMode::Tile // 无害默认，实际不用（effective_dist 短路）
            } else if probe_dist {
                let mut best = DistMode::Tile;
                let mut best_ticks = u64::MAX;
                if static_valid && acc_static < best_ticks {
                    best = DistMode::Static;
                    best_ticks = acc_static;
                }
                if tile_valid && acc_tile < best_ticks {
                    best = DistMode::Tile;
                    best_ticks = acc_tile;
                }
                if dynamic_valid && acc_dynamic < best_ticks {
                    best = DistMode::Dynamic;
                    best_ticks = acc_dynamic;
                }
                best
            } else {
                fixed_dist.unwrap_or(DistMode::Tile)
            };
        // Blt 冠军吞掉 workers 记忆（无意义，不记，避免误导）。
        let persist_workers = if winner_render == RenderMode::Direct && probe_workers {
            Some(winner_workers)
        } else {
            None
        };
        // 单核冠军 / MP 全失败吞掉 dist 记忆（无实测不记，避免误导）。
        let persist_dist = if winner_render == RenderMode::Direct
            && winner_workers == WorkersMode::All
            && probe_dist
            && (static_valid || tile_valid || dynamic_valid)
        {
            Some(winner_dist)
        } else {
            None
        };

        // ---- vsync 探测：冠军路按 60fps 预算跑 `frames` 帧，零超期才开 ----
        let mut overruns = 0u64;
        let winner_vsync = if probe_vsync {
            let budget = tps.max(1) / config::tune_vsync_fps().max(1) as u64;
            if budget == 0 {
                VsyncMode::Off
            } else {
                let use_mp = winner_render == RenderMode::Direct
                    && winner_workers == WorkersMode::All
                    && mp_drive.is_some();
                let use_tile = use_mp && winner_dist == DistMode::Tile;
                let use_dynamic = use_mp && winner_dist == DistMode::Dynamic;
                for _ in 0..frames {
                    let fstart = tsc();
                    let mut frame_over = false;
                    if winner_render == RenderMode::Blt {
                        let _ = self.upload_blt(w, h, &pixels);
                    } else if use_dynamic {
                        frame_over = !mp_drive.as_mut().expect("mp drive").run_round_dynamic();
                    } else if use_tile {
                        frame_over = !mp_drive.as_mut().expect("mp drive").run_round_tiled();
                    } else if use_mp {
                        frame_over = !mp_drive.as_mut().expect("mp drive").run_round();
                    } else {
                        let _ = self.upload_direct(w, h, &pixels);
                    }
                    if !frame_over && tsc().saturating_sub(fstart) > budget {
                        frame_over = true;
                    }
                    if frame_over {
                        overruns += 1;
                    }
                    while tsc().saturating_sub(fstart) < budget {
                        core::hint::spin_loop();
                    }
                }
                if overruns == 0 {
                    VsyncMode::On
                } else {
                    VsyncMode::Off
                }
            }
        } else if config::vsync_mode() == VsyncMode::Auto {
            Some(config::measured_vsync())
                .filter(|v| *v != VsyncMode::Auto)
                .unwrap_or(VsyncMode::Off)
        } else {
            config::vsync_mode()
        };

        // AP 退场（发 QUIT 后给一个循环周期退出，免得 Box 泄漏的 ctx 被踩）。
        cal_round.store(CAL_QUIT, Ordering::SeqCst);
        drop(mp_drive);
        stall(Duration::from_millis(50));

        let cell_str = |timed: bool, acc: u64| {
            if timed && acc > 0 {
                acc.to_string()
            } else {
                String::from("SKIP")
            }
        };
        log::info!(
            "render auto: direct_bsp {} ticks vs mp_static {} vs mp_tile {} vs mp_dynamic {} vs blt {} ({} frames, {} cores) -> render={:?} workers={:?} dist={:?} vsync={:?} (overruns {}/{})",
            if time_direct {
                acc_direct.to_string()
            } else {
                String::from("SKIP")
            },
            cell_str(time_static, acc_static),
            cell_str(time_tile, acc_tile),
            cell_str(time_dynamic, acc_dynamic),
            cell_str(time_blt, acc_blt),
            frames,
            n_cores,
            winner_render,
            winner_workers,
            winner_dist,
            winner_vsync,
            overruns,
            if probe_vsync { frames } else { 0 },
        );
        self.draw_str(&alloc::format!(
            "render auto: {:?}/{:?}/{:?}/vsync {:?} ({} cores)",
            winner_render,
            winner_workers,
            winner_dist,
            winner_vsync,
            n_cores
        ));

        if let Err(e) = config::persist_render_measured(
            probe_render.then_some(winner_render),
            persist_workers,
            persist_dist,
            probe_vsync.then_some(winner_vsync),
            n_cores as u32,
        ) {
            log::warn!("render auto: persist failed: {:?}", e);
        }
    }
}

/// 时间戳计数。非 x86_64 返回 0（auto 会退回 direct）。
#[inline(always)]
pub(crate) fn tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// TSC 频率校准（100ms × 2，和 `hw_bench` 同方法）。vsync 60fps 预算用。
pub(crate) fn calibrate_tps() -> u64 {
    uefi::boot::stall(Duration::from_millis(100));
    let s = tsc();
    uefi::boot::stall(Duration::from_millis(100));
    tsc().saturating_sub(s).saturating_mul(10).max(1)
}

// ---------- 矩阵校准的多核会话 ----------

/// AP 退场哨兵：BSP 写 QUIT 后 AP 跳出常驻循环（同 `gop_bench::FILL_QUIT` 做法）。
const CAL_QUIT: usize = usize::MAX;

/// 模式：0=static, 1=tile, 2=dynamic（三 cell 共用一个会话，轮次间翻转）。
const CAL_MODE_STATIC: u8 = 0;
const CAL_MODE_TILED: u8 = 1;
const CAL_MODE_DYNAMIC: u8 = 2;

struct MpCalCtx<'a> {
    mp: &'a MpServices,
    fb_base: *mut u8,
    fb_stride: usize,
    src: *const u8,
    row_bytes: usize,
    height: usize,
    n: usize,
    round: &'a AtomicUsize,
    done: &'a AtomicUsize,
    mode: &'a AtomicU8,
    tile: &'a AtomicUsize,
    tile_rows: usize,
    tiles_total: usize,
    /// 各核上一轮静态 copy 耗时（AP 自记；BSP 读来算 dynamic 加权边界）。
    times: *const AtomicUsize,
    /// dynamic 加权行边界（n+1 项；BSP 写、mode/round 发布后 AP 只读）。
    bounds: *const AtomicUsize,
}

/// AP 常驻循环：等新轮次号，按本轮模式拷（静态分区 / tile 游标领活 /
/// dynamic 加权边界），打卡。静态分支自带计时（dynamic 定权 probe 用）。
/// BSP 发 QUIT 后退出。mode/bounds 由 BSP 在发布轮次号前写好
///（SeqCst，先 mode/bounds 后 round）。
extern "efiapi" fn ap_cal_copy(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const MpCalCtx) };
    let id = ctx.mp.who_am_i().unwrap_or(usize::MAX) % ctx.n.max(1);
    let (y0, y1) = rows_for_cal(id, ctx.n, ctx.height);
    let mut my_round = 0usize;
    loop {
        let g = ctx.round.load(Ordering::Acquire);
        if g == CAL_QUIT {
            break;
        }
        if g == 0 || g == my_round {
            core::hint::spin_loop();
            continue;
        }
        my_round = g;
        let mode = ctx.mode.load(Ordering::Acquire);
        if mode == CAL_MODE_TILED {
            let base = (g as u64).saturating_mul(ctx.tiles_total as u64);
            loop {
                let t = ctx.tile.fetch_add(1, Ordering::SeqCst) as u64;
                let rel = t.saturating_sub(base) as usize;
                if rel >= ctx.tiles_total {
                    break;
                }
                let ty0 = rel * ctx.tile_rows;
                let ty1 = (ty0 + ctx.tile_rows).min(ctx.height);
                unsafe {
                    copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, ty0, ty1)
                };
            }
        } else if mode == CAL_MODE_DYNAMIC {
            let dy0 = unsafe {
                (*ctx.bounds.add(id))
                    .load(Ordering::Acquire)
                    .min(ctx.height)
            };
            let dy1 = unsafe {
                (*ctx.bounds.add(id + 1))
                    .load(Ordering::Acquire)
                    .min(ctx.height)
                    .max(dy0)
            };
            unsafe { copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, dy0, dy1) };
        } else {
            let t0 = tsc();
            unsafe { copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, y0, y1) };
            unsafe {
                (*ctx.times.add(id)).store(tsc().saturating_sub(t0) as usize, Ordering::Relaxed)
            };
        }
        ctx.done.fetch_add(1, Ordering::SeqCst);
    }
}

/// BSP 侧驱动器：发轮次号 → 拷自己份（分区/领活/加权边界）→ 等 AP 打卡齐（单轮 5 秒熔断）。
struct MpDrive<'a> {
    n: usize,
    bsp_id: usize,
    fb_base: *mut u8,
    fb_stride: usize,
    src: *const u8,
    row_bytes: usize,
    height: usize,
    bsp_y0: usize,
    bsp_y1: usize,
    expected: usize,
    round: &'a AtomicUsize,
    done: &'a AtomicUsize,
    mode: &'a AtomicU8,
    tile: &'a AtomicUsize,
    tile_rows: usize,
    tiles_total: usize,
    times: *const AtomicUsize,
    bounds: *const AtomicUsize,
    next: usize,
    tps: u64,
}

impl MpDrive<'_> {
    /// 热身 1 轮静态分区（吸收 AP 唤醒延迟，不计时）。
    fn warmup_static(&mut self) {
        let _ = self.run_round();
    }

    /// 热身 1 轮 tile（同上）。
    fn warmup_tiled(&mut self) {
        let _ = self.run_round_tiled();
    }

    /// 热身 1 轮 dynamic（定权 probe 之后，不计时）。
    fn warmup_dynamic(&mut self) {
        let _ = self.run_round_dynamic();
    }

    /// dynamic 定权 probe：跑一轮静态（AP 在静态分支自带计时），读各核耗时
    /// 算加权边界并发布。返回 false = 本轮 AP 没跟上（调用方作废整个 MP 会话）。
    fn probe_dynamic(&mut self) -> bool {
        if !self.run_round() {
            return false;
        }
        let mut times = vec![0u64; self.n.max(1)];
        for (i, t) in times.iter_mut().enumerate() {
            *t = unsafe { (*self.times.add(i)).load(Ordering::Relaxed) } as u64;
        }
        let b = dynamic_row_bounds(&times, self.n, 0, self.height);
        for (i, v) in b.iter().enumerate() {
            unsafe { (*self.bounds.add(i)).store(*v, Ordering::Relaxed) };
        }
        true
    }

    /// 跑一轮 dynamic 加权分区（BSP 拷自己的加权行区；AP 按发布的边界拷）。
    fn run_round_dynamic(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        self.mode.store(CAL_MODE_DYNAMIC, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        let dy0 = unsafe { (*self.bounds.add(self.bsp_id)).load(Ordering::Relaxed) };
        let dy1 = unsafe { (*self.bounds.add(self.bsp_id + 1)).load(Ordering::Relaxed) };
        let dy0 = dy0.min(self.height);
        let dy1 = dy1.min(self.height).max(dy0);
        unsafe {
            copy_rows_cal(
                self.fb_base,
                self.fb_stride,
                self.src,
                self.row_bytes,
                dy0,
                dy1,
            )
        };
        self.wait_done(r)
    }

    /// 跑一轮静态分区，返回是否全员跟上（false = 熔断，调用方作废 MP 双 cell）。
    fn run_round(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        self.mode.store(CAL_MODE_STATIC, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        unsafe {
            copy_rows_cal(
                self.fb_base,
                self.fb_stride,
                self.src,
                self.row_bytes,
                self.bsp_y0,
                self.bsp_y1,
            )
        };
        self.wait_done(r)
    }

    /// 跑一轮 tile 自调度（游标按轮次编址 `[r*N, (r+1)*N)`，BSP 也领活）。
    fn run_round_tiled(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
        let base = (r as u64).saturating_mul(self.tiles_total as u64);
        self.tile.store(base as usize, Ordering::SeqCst);
        self.mode.store(CAL_MODE_TILED, Ordering::SeqCst);
        self.round.store(r, Ordering::SeqCst);
        loop {
            let t = self.tile.fetch_add(1, Ordering::SeqCst) as u64;
            let rel = t.saturating_sub(base) as usize;
            if rel >= self.tiles_total {
                break;
            }
            let y0 = rel * self.tile_rows;
            let y1 = (y0 + self.tile_rows).min(self.height);
            unsafe {
                copy_rows_cal(
                    self.fb_base,
                    self.fb_stride,
                    self.src,
                    self.row_bytes,
                    y0,
                    y1,
                )
            };
        }
        self.wait_done(r)
    }

    /// 等本轮 AP 打卡齐（5 秒熔断）。
    fn wait_done(&self, r: usize) -> bool {
        let target = r.saturating_mul(self.expected);
        let deadline = tsc().saturating_add(5 * self.tps.max(1));
        while self.done.load(Ordering::Acquire) < target {
            if tsc() >= deadline {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }
}

/// 按核 id 划分行区，尾核吃掉余数（同 `gop_bench::rows_for`）。
fn rows_for_cal(id: usize, n: usize, h: usize) -> (usize, usize) {
    let per = h / n.max(1);
    let y0 = id * per;
    let y1 = if id + 1 >= n { h } else { y0 + per };
    (y0.min(h), y1.min(h))
}

/// 按各核搬运耗时加权分行（dynamic 用；校准定权与播放自适应共用同一数学）。
/// `times[i]` 为核 i 上一轮 copy ticks（0 = 无数据）；`y0..y1` 为待分行区
/// （播放侧传 `OSD_ROWS..H`，校准侧传全高）。快核多拿：
/// 行数正比于速率（`SCALE / t`），最大余数法 + 每核至少 1 行（行数够分时）。
/// 全无数据 → 等分；核比行多（退化，实机到不了）→ 前面的核各 1 行。
/// 返回 n+1 项边界（首项 y0、末项 y1）。整数运算，无浮点（软浮点历史，见 AGENTS）。
pub(crate) fn dynamic_row_bounds(times: &[u64], n: usize, y0: usize, y1: usize) -> Vec<usize> {
    let n = n.max(1);
    let total = y1.saturating_sub(y0);
    let mut bounds = Vec::with_capacity(n + 1);
    bounds.push(y0);
    if total == 0 {
        for _ in 0..n {
            bounds.push(y0);
        }
        return bounds;
    }
    // 速率：耗时越小越快。0 耗时视为无数据（首轮/异常），先记 0。
    const SCALE: u64 = 1 << 24;
    let mut rates = vec![0u64; n];
    for (i, r) in rates.iter_mut().enumerate() {
        let t = times.get(i).copied().unwrap_or(0);
        if t > 0 {
            *r = SCALE / t.max(1);
        }
    }
    if rates.iter().all(|&r| r == 0) {
        // 全无数据 → 等分（首轮就这样，与 static 同分布）。
        for i in 1..=n {
            bounds.push(y0 + total * i / n);
        }
        return bounds;
    }
    // 部分无数据的核按平均速率兜底（防 0 行震荡：0 行 → 耗时 0 → 下轮爆权重）。
    let known: Vec<u64> = rates.iter().copied().filter(|&r| r > 0).collect();
    let avg = known.iter().sum::<u64>() / known.len().max(1) as u64;
    for r in rates.iter_mut() {
        if *r == 0 {
            *r = avg.max(1);
        }
    }
    let sum: u64 = rates.iter().sum();
    let mut rows = vec![0usize; n];
    if total >= n {
        // 先每核预留 1 行，剩余按权重最大余数分。
        let rest = (total - n) as u64;
        let mut fracs = vec![0u64; n];
        let mut assigned = 0usize;
        for (i, r) in rates.iter().enumerate() {
            let base = rest * r / sum.max(1);
            fracs[i] = rest * r % sum.max(1);
            rows[i] = 1 + base as usize;
            assigned += base as usize;
        }
        let mut leftover = rest as usize - assigned;
        while leftover > 0 {
            let mut pick = 0;
            for i in 1..n {
                if fracs[i] > fracs[pick] {
                    pick = i;
                }
            }
            if fracs[pick] == 0 && leftover < n {
                break;
            }
            rows[pick] += 1;
            fracs[pick] = 0;
            leftover -= 1;
        }
    } else {
        for (i, r) in rows.iter_mut().enumerate() {
            *r = if i < total { 1 } else { 0 };
        }
    }
    let mut y = y0;
    for r in rows {
        y += r;
        bounds.push(y.min(y1));
    }
    // 末项钳到 y1（整除余数场景，rows 总和恒等于 total，钳制只是兜底）。
    if let Some(last) = bounds.last_mut() {
        *last = y1;
    }
    bounds
}

/// 从合成帧行区拷到显存行区（显存 stride 可能大于行宽，逐行拷）。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
unsafe fn copy_rows_cal(
    dst_base: *mut u8,
    dst_stride: usize,
    src_frame: *const u8,
    row_bytes: usize,
    y0: usize,
    y1: usize,
) {
    unsafe {
        for y in y0..y1 {
            let d = dst_base.add(y * dst_stride);
            let s = src_frame.add(y * row_bytes);
            core::ptr::copy_nonoverlapping(s, d, row_bytes);
        }
    }
}

/// P2 位宽分发：安全外壳 + 运行时门控（调用方保证 CPU 支持该位宽，
/// 见 `config::copy_width`），内部分 unsafe 执行。AVX 系自带 `vzeroupper`。
/// `video::mp_draw` 的 AP/BSP 行区搬运复用同一个（同 semantics）。
pub(crate) fn copy_block(dst: *mut u8, src: *const u8, len: usize, w: CopyWidth) {
    unsafe {
        match w {
            CopyWidth::U64 => copy_block_u64(dst, src, len),
            CopyWidth::X128 => copy_block_x128(dst, src, len),
            CopyWidth::V128 => copy_block_v128(dst, src, len),
            CopyWidth::Y256 => copy_block_y256(dst, src, len),
            CopyWidth::Z512 => copy_block_z512(dst, src, len),
        }
    }
}

/// 标量兜底（`accel.level = scalar` 或史前 CPU）：u64 步进。
unsafe fn copy_block_u64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            (dst.add(i) as *mut u64).write((src.add(i) as *const u64).read_unaligned());
            i += 8;
        }
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 128 位整数向量（SSE2~SSE4.2 同宽）：`movdqu` 非对齐载存。
/// 约束见 `gop_simd.rs` 头注释：只碰整数 `asm!`，内在函数会被标量化。
#[target_feature(enable = "sse2")]
unsafe fn copy_block_x128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 128 位 VEX 编码（`accel.level = avx`）：同宽换 VEX，对照语义。
#[target_feature(enable = "avx")]
unsafe fn copy_block_v128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 256 位向量（`accel.level = avx2`）：调用前必须过 CPUID+XCR0 门控。
#[target_feature(enable = "avx2")]
unsafe fn copy_block_y256(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 512 位向量（`accel.level = avx512`）：调用前必须过 AVX512F+XCR0 五位门控。
#[target_feature(enable = "avx512f")]
unsafe fn copy_block_z512(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovdqu8 [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        while i < len {
            dst.add(i).write(src.add(i).read());
            i += 1;
        }
    }
}

/// 极致性能的底层拷贝函数（无 self 依赖）
#[inline(always)]
unsafe fn u64_fast_copy(src: *const u8, dst: *mut u8, len: usize) {
    let mut i = 0;
    while i + 64 <= len {
        let s0 = (src.add(i) as *const u64).read_unaligned();
        let s1 = (src.add(i + 8) as *const u64).read_unaligned();
        let s2 = (src.add(i + 16) as *const u64).read_unaligned();
        let s3 = (src.add(i + 24) as *const u64).read_unaligned();
        let s4 = (src.add(i + 32) as *const u64).read_unaligned();
        let s5 = (src.add(i + 40) as *const u64).read_unaligned();
        let s6 = (src.add(i + 48) as *const u64).read_unaligned();
        let s7 = (src.add(i + 56) as *const u64).read_unaligned();

        (dst.add(i) as *mut u64).write_volatile(s0);
        (dst.add(i + 8) as *mut u64).write_volatile(s1);
        (dst.add(i + 16) as *mut u64).write_volatile(s2);
        (dst.add(i + 24) as *mut u64).write_volatile(s3);
        (dst.add(i + 32) as *mut u64).write_volatile(s4);
        (dst.add(i + 40) as *mut u64).write_volatile(s5);
        (dst.add(i + 48) as *mut u64).write_volatile(s6);
        (dst.add(i + 56) as *mut u64).write_volatile(s7);
        i += 64;
    }
    while i + 8 <= len {
        (dst.add(i) as *mut u64).write_volatile((src.add(i) as *const u64).read_unaligned());
        i += 8;
    }
    while i < len {
        dst.add(i).write_volatile(src.add(i).read());
        i += 1;
    }
}
