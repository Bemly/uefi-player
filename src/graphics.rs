use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use core::arch::asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use uefi::boot::{create_event, get_handle_for_protocol, open_protocol_exclusive, stall, EventType, ScopedProtocol, Tpl};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, Mode};
use uefi::proto::pi::mp::MpServices;
use crate::config::{self, CopyWidth, RenderMode, VsyncMode, WorkersMode};
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

    /// `render.mode/workers/vsync = "auto"` 的开机矩阵校准（`\player.toml` 驱动）。
    ///
    /// - 首启（任一 auto 无记忆，见 `config::needs_render_bench`）必跑全矩阵：
    ///   直写 vs Blt 取快；直写路再比 BSP 单核 vs 全核并行；冠军路按 60fps
    ///   预算跑一遍，零超期则 vsync 开否则关。
    /// - 全核 cell 用 AP 常驻循环 + 轮次同步（`gop_bench` 方法论：单发 startup
    ///   测到的是 SIPI 唤醒开销而非扩展性，必须常驻 + 热身吸收后再计时）。
    /// - 同组变体轮转交替计时（`gop_simd` 方法论：杀时间漂移），结论写回
    ///   `measured_*`（`None` 维度不动盘），下次启动有记忆直接复用快启。
    /// - 会短暂刷花屏（随后菜单/播放会重画）。
    pub fn ensure_render_calibrated(&mut self) {
        if !config::needs_render_bench() {
            config::apply_measured_render();
            log::info!(
                "render auto: reuse measured render={:?} workers={:?} vsync={:?} cores={}",
                config::measured_render(),
                config::measured_workers(),
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
                                fb_base,
                                fb_stride,
                                src: pixels.as_ptr() as *const u8,
                                row_bytes: w * 4,
                                bsp_y0: by0,
                                bsp_y1: by1,
                                expected: n_cores.saturating_sub(1),
                                round: &cal_round,
                                done: &cal_done,
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
        // 多核热身 1 轮（吸收 AP 唤醒延迟，不计时）。
        if time_mp {
            mp_drive.as_mut().expect("mp drive").warmup();
        }
        let mut acc_direct = 0u64;
        let mut acc_mp = 0u64;
        let mut acc_blt = 0u64;
        for _ in 0..frames {
            if time_direct {
                let t = tsc();
                let _ = self.upload_direct(w, h, &pixels);
                acc_direct += tsc().saturating_sub(t);
            }
            if time_mp {
                let t = tsc();
                let ok = mp_drive.as_mut().expect("mp drive").run_round();
                let dt = tsc().saturating_sub(t);
                if ok {
                    acc_mp += dt;
                } else {
                    // 熔断：本轮 AP 没跟上，该 cell 作废并退回单核。
                    acc_mp = 0;
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
        let time_mp = time_mp && mp_drive.is_some();

        // ---- 决胜 ----
        // 直写冠军：多核 cell 跳过/失败则退回单核。
        let mp_valid = time_mp && acc_mp > 0;
        let direct_ticks = if mp_valid && acc_mp < acc_direct {
            acc_mp
        } else {
            acc_direct
        };
        let winner_workers = if !direct_in_play {
            WorkersMode::BspOnly // Blt 路径恒单线程，workers 无意义
        } else if probe_workers {
            if mp_valid && acc_mp < acc_direct {
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
        // Blt 冠军吞掉 workers 记忆（无意义，不记，避免误导）。
        let persist_workers = if winner_render == RenderMode::Direct && probe_workers {
            Some(winner_workers)
        } else {
            None
        };

        // ---- vsync 探测：冠军路按 60fps 预算跑 `frames` 帧，零超期才开 ----
        let mut overruns = 0u64;
        let winner_vsync = if probe_vsync {
            let budget = tps / 60;
            if budget == 0 {
                VsyncMode::Off
            } else {
                let use_mp = winner_render == RenderMode::Direct
                    && winner_workers == WorkersMode::All
                    && mp_drive.is_some();
                for _ in 0..frames {
                    let fstart = tsc();
                    let mut frame_over = false;
                    if winner_render == RenderMode::Blt {
                        let _ = self.upload_blt(w, h, &pixels);
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

        log::info!(
            "render auto: direct_bsp {} ticks vs direct_all {} vs blt {} ({} frames, {} cores) -> render={:?} workers={:?} vsync={:?} (overruns {}/{})",
            acc_direct,
            if mp_valid {
                acc_mp.to_string()
            } else {
                String::from("SKIP")
            },
            acc_blt,
            frames,
            n_cores,
            winner_render,
            winner_workers,
            winner_vsync,
            overruns,
            if probe_vsync { frames } else { 0 },
        );
        self.draw_str(&alloc::format!(
            "render auto: {:?}/{:?}/vsync {:?} ({} cores)",
            winner_render,
            winner_workers,
            winner_vsync,
            n_cores
        ));

        if let Err(e) = config::persist_render_measured(
            probe_render.then_some(winner_render),
            persist_workers,
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
}

/// AP 常驻循环：等新轮次号，拷自己行区，打卡。BSP 发 QUIT 后退出。
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
        unsafe { copy_rows_cal(ctx.fb_base, ctx.fb_stride, ctx.src, ctx.row_bytes, y0, y1) };
        ctx.done.fetch_add(1, Ordering::SeqCst);
    }
}

/// BSP 侧驱动器：发轮次号 → 拷自己行区 → 等 AP 打卡齐（单轮 5 秒熔断）。
struct MpDrive<'a> {
    fb_base: *mut u8,
    fb_stride: usize,
    src: *const u8,
    row_bytes: usize,
    bsp_y0: usize,
    bsp_y1: usize,
    expected: usize,
    round: &'a AtomicUsize,
    done: &'a AtomicUsize,
    next: usize,
    tps: u64,
}

impl MpDrive<'_> {
    /// 热身 1 轮（吸收 AP 唤醒延迟，不计时）。
    fn warmup(&mut self) {
        let _ = self.run_round();
    }

    /// 跑一轮，返回是否全员跟上（false = 熔断，调用方作废该 cell）。
    fn run_round(&mut self) -> bool {
        self.next += 1;
        let r = self.next;
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
            (dst.add(i) as *mut u64)
                .write((src.add(i) as *const u64).read_unaligned());
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