use alloc::boxed::Box;
use alloc::{format, vec};
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use uefi::boot::{create_event, get_handle_for_protocol, open_protocol_exclusive, set_watchdog_timer, EventType, Tpl};
use uefi::{boot, cstr16, CStr16, Status};
use uefi::proto::console::gop::BltPixel;
use uefi::proto::media::file::{File, FileInfo, RegularFile};
use uefi::proto::pi::mp::MpServices;
use crate::config;
use crate::fs::Fs;
use crate::graphics::Screen;
use crate::video::buffer::{BltFrameBuffer, QoiFrameBuffer, RawFrameBuffer};
use crate::video::decoder::{VideoMemory, VideoMemoryRaw};
use crate::error::Result;
use crate::video::ascii_font::FONT_8X16;

pub mod buffer;
pub mod decoder;
pub mod ascii_font;

const VIDEO_W: usize = 1920;
const VIDEO_H: usize = 1080;
/// `.qois` 无帧率元数据，沿用 `mp_draw` 的生产假设：120Hz 视频。
/// `auto` 的上限就按它封顶（比它快也没意义）。
const VIDEO_NATIVE_FPS: u32 = 120;

#[inline(always)]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC 频率校准（100ms × 2，和 `hw_bench` 同方法，QEMU/真机都可用）。
fn calibrate_tps() -> u64 {
    boot::stall(Duration::from_millis(100));
    let s = rdtsc();
    boot::stall(Duration::from_millis(100));
    rdtsc().saturating_sub(s).saturating_mul(10).max(1)
}

/// 限速到目标帧率：从帧起始自旋到预算。`None` = 不限速（max 模式）。
#[inline]
fn pace_frame(fstart: u64, tps: u64, target: Option<u32>) {
    if let Some(fps) = target {
        let budget = tps.max(1) / fps.max(1) as u64;
        while rdtsc().saturating_sub(fstart) < budget {
            core::hint::spin_loop();
        }
    }
}


pub fn video_run(screen: &mut Screen) -> Result {

    // 关闭看门狗，如果不行之后写定时喂狗
    set_watchdog_timer(0, 0, None)?;

    let tps = calibrate_tps();
    let target = resolve_play_target(screen, tps)?;

    let mut fs = Fs::new()?;
    let file = fs.open_file(cstr16!("1080p\\video.qois"))?;

    // 渲染路径矩阵（\palyer.toml，见 `Screen::ensure_render_calibrated`）：
    //   blt            → 单线程走 GOP Blt 引擎（workers 无意义）
    //   direct + bsp   → BSP 单核直写（MP 关闭或校准判单核更快）
    //   direct + all   → 原来的多核直写
    match crate::config::effective_render() {
        crate::config::RenderMode::Blt => blt_loop(screen, file, VIDEO_W, VIDEO_H, target, tps),
        _ => match crate::config::effective_workers() {
            crate::config::WorkersMode::BspOnly => {
                direct_loop(screen, file, VIDEO_W, VIDEO_H, target, tps)
            }
            _ => {
                let mut file = file;
                mp_draw(screen, &mut file, VIDEO_W, VIDEO_H, target, tps)
            }
        },
    }
}

/// 播放帧率决策：max 不限速；capped 直接用；auto 无实测就现测并回写，
/// 按 `min(视频帧率, 实测*3/4)` 跑。返回 `None` = 不限速。
/// 最后叠 vsync 开关：开 = 再封顶 60fps（软件帧预算，GOP 无硬件 vsync）。
fn resolve_play_target(screen: &mut Screen, tps: u64) -> Result<Option<u32>> {
    let base = match config::play_policy() {
        config::FpsPolicy::Max => None,
        config::FpsPolicy::Capped(n) => Some(n),
        config::FpsPolicy::Auto => {
            let m = config::measured_max_fps();
            let m = if m == 0 {
                let mm = measure_max_fps(screen, tps)?;
                config::persist_measured_max_fps(mm)?;
                mm
            } else {
                m
            };
            Some((m as u64 * 3 / 4).min(VIDEO_NATIVE_FPS as u64).max(1) as u32)
        }
    };
    if config::vsync_enabled() {
        Ok(Some(base.map(|t| t.min(60)).unwrap_or(60)))
    } else {
        Ok(base)
    }
}

/// 首启实测：全量预解码后，用生效渲染路径整轮推一遍，算最大播放帧率。
/// 纯展示性快进（和开机 render bench 一样会刷屏），一次性的，结果回写配置。
/// 注意这里是单线程 `upload_frame` 推送：workers=all 的真实多核路只会更快，
/// 测出来是保守下限，auto 取 3/4 本来就有余量。
fn measure_max_fps(screen: &mut Screen, tps: u64) -> Result<u32> {
    screen.draw_str("play: benching max playback fps (one fast pass)...");
    let mut fs = Fs::new()?;
    let file = fs.open_file(cstr16!("1080p\\video.qois"))?;
    let mut video = VideoMemoryRaw::new(file)?;
    let frames = video.frames.len().max(1);
    let t0 = rdtsc();
    for _ in 0..frames {
        match video.next_frame() {
            Some(f) => screen.upload_frame(VIDEO_W, VIDEO_H, f)?,
            None => break,
        }
    }
    let dt = rdtsc().saturating_sub(t0).max(1);
    let fps = ((frames as u64 * tps) / dt).clamp(1, 10_000) as u32;
    screen.draw_str(&format!("play: max {} fps, running at 3/4 (capped by video)", fps));
    Ok(fps)
}

/// 单线程 Blt 播放：`VideoMemoryRaw` 预解码全部帧（含 RGBA→BGRA），
/// 每帧经 `upload_frame` 交给 GOP Blt 引擎。牺牲多核并行，换引擎搬运。
fn blt_loop(
    screen: &mut Screen,
    file: RegularFile,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut video = VideoMemoryRaw::new(file)?;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => video.rewind(),
        }
        pace_frame(fstart, tps, target);
    }
}

/// BSP 单核直写播放：与 `blt_loop` 同结构，只是走 CPU 直写显存
///（校准判单核更快，或用户关掉 MP 时用；`upload_frame` 按生效路径分发，
/// 这里 render 已定 direct，实际就是 `upload_direct`）。
fn direct_loop(
    screen: &mut Screen,
    file: RegularFile,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut video = VideoMemoryRaw::new(file)?;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => video.rewind(),
        }
        pace_frame(fstart, tps, target);
    }
}

fn draw_once(fs: &mut Fs, screen: &mut Screen, path: &CStr16, blt_buf: &mut [BltPixel]) -> Result {
    // 加载文件
    let qoi_data = fs.read_file(path)?;

    // 解码图像
    let (header, rgba_buf) = qoi::decode_to_vec(&qoi_data)?;

    // 转换像素格式
    blt_buf.iter_mut()
        .zip(rgba_buf.chunks_exact(header.channels.as_u8() as usize))
        .for_each(|(dest, src)| {
            *dest = BltPixel::new(src[0], src[1], src[2]);
        });

    // 渲染
    screen.draw_image(header.width, header.height, &blt_buf)?;

    Ok(())
}

fn draw_all_mem(
    video: &mut VideoMemory,
    screen: &mut Screen,
    qoi: &mut QoiFrameBuffer,
    raw: &mut RawFrameBuffer,
    blt: &mut BltFrameBuffer
) -> Result {
    if video.next_frame(&mut qoi.0) {
        raw.header = loop {
            match qoi::decode_to_buf(&mut raw.pixels, &qoi.0) {
                Ok(header) => break header,
                Err(qoi::Error::OutputBufferTooSmall { required, .. }) => {
                    raw.pixels.resize(required, 0)
                }
                // TODO:严重错误 真机上会出现数据错位的情况,概率极大
                Err(e) => return Ok(()),
            }
        };

        let pixel_count = raw.get_size();
        let step = raw.header.channels.as_u8() as usize;

        // 这里的 zip 配合 iter_mut 是目前最稳妥的写法
        raw.pixels[..pixel_count * step]
            .chunks_exact(step)
            .zip(blt.0[..pixel_count].iter_mut())
            .for_each(|(r, b)| {
                b.red = r[0];
                b.green = r[1];
                b.blue = r[2];
            });

        // 4. 显示
        screen.draw_image(raw.header.width, raw.header.height, &blt.0)?;
    } else {
        // 读完了，重置指针实现循环播放
        video.rewind();
    }

    Ok(())
}

// 目前只支持4通道互转
fn draw_all_mem_zero_copy(
    video: &mut VideoMemory,
    screen: &mut Screen,
    qoi: &mut QoiFrameBuffer,
    blt: &mut BltFrameBuffer
) -> Result {
    if video.next_frame(&mut qoi.0) {
        // TODO:严重错误 真机上会出现数据错位的情况,概率极大
        let Ok(header) = qoi::decode_header(&qoi.0) else { return Ok(()) };
        let pixel_count = (header.width * header.height) as usize;

        if blt.0.len() < pixel_count {
            blt.0.resize(pixel_count, BltPixel::new(0,0,0));
        }

        // 直接解码到 [R, G, B, A, R, G, B, A...]
        // TODO:严重错误 真机上会出现数据错位的情况,概率极大
        let Ok(_) = qoi::decode_to_buf(as_u8_slice_mut(&mut blt.0[..pixel_count]), &qoi.0)
        else { return Ok(()) };

        // 交换 R 和 B
        for pixel in blt.0[..pixel_count].iter_mut() {
            core::mem::swap(&mut pixel.red, &mut pixel.blue);
        }

        screen.draw_image(header.width, header.height, &blt.0)?;
    } else {
        video.rewind();
    }
    Ok(())
}

fn as_u8_slice_mut(slice: &mut [BltPixel]) -> &mut [u8] {
    let len = slice.len() * core::mem::size_of::<BltPixel>();
    unsafe {
        // 起始地址（指针转换） 总字节长度
        core::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut u8, len)
    }
}

fn draw(
    fs: &mut Fs,
    file: &mut RegularFile,
    screen: &mut Screen,
    qoi: &mut QoiFrameBuffer,
    raw: &mut RawFrameBuffer,
    blt: &mut BltFrameBuffer
) -> Result {
    // 读文件流
    if fs.read_frame_next(file, &mut qoi.0)? {
        // 解码
        raw.header = loop {
            match qoi::decode_to_buf(&mut raw.pixels, &qoi.0) {
                Ok(header) => break header,
                Err(qoi::Error::OutputBufferTooSmall { required, .. }) => {
                    raw.pixels.resize(required, 0)
                }
                // TODO:严重错误 真机上会出现数据错位的情况,概率极大
                // qoi::Error::InvalidPadding
                // qemu无问题 不知道怎么解决 暂时丢帧处理
                Err(e) => return Ok(()),
            }
        };

        // 转换
        // 只取当前帧需要的切片范围，避免处理旧数据
        let step = raw.header.channels.as_u8() as usize;
        let raw_data = raw.pixels[..raw.get_size() * step].chunks_exact(step);
        let blt_data = &mut blt.0[..raw.get_size()];
        for (r, b) in raw_data.zip(blt_data.iter_mut()) {
            b.red = r[0];
            b.green = r[1];
            b.blue = r[2];
        }

        screen.draw_image(raw.header.width, raw.header.height, &blt.0)?;
    } else {
        // 从头读
        file.set_position(0)?;
    }

    Ok(())
}

type PixelPoint = u8;
type Frame = Vec<PixelPoint>;
type FrameSegment = Vec<Frame>;
type CoreFrameSegment = Vec<FrameSegment>;

#[repr(C)]
struct PlayTask<'a> {
    mp: &'a MpServices, // 用于 who_am_i
    fb_base: *mut u8,
    stride_bytes: usize,
    width: usize,
    height: usize,
    num_cores: usize,
    // core_frames[核心ID][帧ID] -> 这一帧该核负责的像素切片
    // 注意：这里需要是指针的指针，因为 AP 无法直接访问 Vec 的元数据
    core_frame_ptrs: *const *const *const u8,
    total_frames: usize,
    sync_counter: &'a AtomicUsize, // 关键：原子计数器
    target_fps: u32, // 0 = 不限速（max 模式）
    tps: u64,
}

extern "efiapi" fn play_task(arg: *mut c_void) {
    if arg.is_null() { return; }
    let ctx = unsafe { &*(arg as *const PlayTask) };

    let my_id = unsafe { (*ctx.mp).who_am_i().unwrap_or(usize::MAX) };
    if my_id >= ctx.num_cores { return; }

    // 核心参数计算
    let rows_per_core = ctx.height / ctx.num_cores;
    let y_start = my_id * rows_per_core;
    let my_fb_ptr = unsafe { ctx.fb_base.add(y_start * ctx.stride_bytes) };
    let my_frames_list = unsafe { *ctx.core_frame_ptrs.add(my_id) };
    let my_block_size = if my_id == ctx.num_cores - 1 {
        (ctx.height - y_start) * ctx.stride_bytes
    } else {
        rows_per_core * ctx.stride_bytes
    };

    let mut local_frame_idx = 0;
    let n_cores = ctx.num_cores;
    // 初始化第一个目标值：第一帧写完时，计数器应该达到 n_cores
    let mut my_next_target = n_cores;

    // 计算帧率
    let ticks_per_sec;
    let target_ticks_per_frame;
    const TARGET_FPS: u128 = 60;
    if my_id == 0 {
        boot::stall(Duration::from_millis(100));
        // 采样时间太短（如 1ms）误差大，太长（如 1s）启动慢，100ms 是黄金平衡点
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        boot::stall(Duration::from_millis(100));
        let end = unsafe { core::arch::x86_64::_rdtsc() };

        ticks_per_sec = ((end - start) * 10) as u128;

        // 如果你跑的是 120Hz 视频，这里必须是 120
        target_ticks_per_frame = ticks_per_sec / 120;
    } else {
        ticks_per_sec = 0;
        target_ticks_per_frame = 0;
    }

    /// 绘制不透明字符串：位图为 1 画 color，位图为 0 画黑色
    unsafe fn draw_string_opaque(fb_base: *mut u32, stride: usize, x: u32, y: u32, s: &[u8], color: u32) {
        let mut current_x = x;
        for &char_code in s {
            // 只有 ASCII 0-127 有效
            let glyph = FONT_8X16[(char_code & 0x7F) as usize];

            for row in 0..16 {
                let row_data = glyph[row];
                let row_ptr = fb_base.add(((y + row as u32) * stride as u32 + current_x) as usize);

                for col in 0..8 {
                    // 直接判断位并写入，不再调用 draw_pixel 减少重复计算
                    // FONT_8X16 通常高位在左：0x80 >> col
                    let pixel_color = if (row_data << col) & 0x80 != 0 {
                        color
                    } else {
                        0x000000 // 背景黑
                    };

                    row_ptr.add(col).write_volatile(pixel_color);
                }
            }
            current_x += 8;
        }
    }

    let mut start_ticks = unsafe { core::arch::x86_64::_rdtsc() };
    let mut fps_counter = 0;
    let mut last_sample_ticks = unsafe { core::arch::x86_64::_rdtsc() };
    let stride = (ctx.stride_bytes / 4) as usize;
    let ui_height = 20;
    let start_y = ui_height;
    let end_y = (ctx.height / 4) as usize;
    let dst_ptr = ctx.fb_base as *mut u32;

    // 计算偏移量和拷贝大小（也是固定的）
    let offset = start_y * stride;
    let copy_size = if end_y > start_y { (end_y - start_y) * stride } else { 0 };
    loop {
        let fstart = rdtsc();
        // 1. 搬运 (生产)
        unsafe {
            if my_id == 0 {
                // 1. 获取当前帧的源地址（这个不能移出去，因为每帧 index 不同）
                let src_frame_base = unsafe { *my_frames_list.add(local_frame_idx) as *const u32 };

                // 2. 镂空搬运
                if copy_size > 0 {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            src_frame_base.add(offset), // 基于当前帧地址偏移
                            dst_ptr.add(offset),        // 基于显存地址偏移
                            copy_size
                        );
                    }
                }
            }else {
                let src_ptr = *my_frames_list.add(local_frame_idx);
                core::ptr::copy_nonoverlapping(src_ptr, my_fb_ptr, my_block_size);
            }

        }

        // 2. 打卡 (原子加法)
        // fetch_add 本身会返回旧值，但我们这里直接加，不关心返回值
        ctx.sync_counter.fetch_add(1, Ordering::SeqCst);

        // 3. 自旋等待 (无除法)
        // 所有人都在等计数器达到本轮的 my_next_target
        while ctx.sync_counter.load(Ordering::Acquire) < my_next_target {
            core::hint::spin_loop();
        }

        // 3.5 限速：栅栏刚对齐过，各核按同一预算自旋（漂移可忽略）。
        // BSP 不特殊处理——它慢下来 AP 也会在下轮栅栏处等齐，整管线自然限速。
        if ctx.target_fps > 0 {
            let budget = ctx.tps.max(1) / ctx.target_fps.max(1) as u64;
            while rdtsc().saturating_sub(fstart) < budget {
                core::hint::spin_loop();
            }
        }

        // 4. 只有 0 号核做统计（可选，且不影响同步）
        if my_id == 0 {
            let end_ticks = unsafe { core::arch::x86_64::_rdtsc() };
            let delta_ticks = (end_ticks - start_ticks) as u128;

            // 1. 计算当前帧耗时和余量
            let ft_us = (delta_ticks * 1_000_000) / ticks_per_sec;
            let margin_us = if target_ticks_per_frame > delta_ticks {
                ((target_ticks_per_frame - delta_ticks) * 1_000_000) / ticks_per_sec
            } else {
                0
            };

            fps_counter += 1;
            // 每 64 帧更新一次显示
            if (fps_counter & 63) == 0 {
                // --- 核心修正：计算被漏掉的 fps ---
                let total_span = end_ticks.saturating_sub(last_sample_ticks);
                // 既然是 64 帧更新一次，公式就是 (64 * 频率) / 总耗时
                let fps = (64 * ticks_per_sec) / (total_span as u128);

                last_sample_ticks = end_ticks;

                let fb = ctx.fb_base as *mut u32;
                // 注意：stride 必须是像素跨度，确保你在 loop 外已经算好了 stride = stride_bytes / 4

                // --- 绘制逻辑 ---
                let fps_str = format!("FPS: {:>4}", fps);
                let ft_str  = format!("FT: {:>5} us", ft_us);
                let mg_str  = format!("Margin: {:>5} us", margin_us);

                unsafe {
                    // 并排显示在最顶层 (y=0)
                    draw_string_opaque(fb, stride, 0,   0, fps_str.as_bytes(), 0x00FF00); // 绿色
                    draw_string_opaque(fb, stride, 200, 0, ft_str.as_bytes(), 0x00FFFF);  // 青色
                    draw_string_opaque(fb, stride, 450, 0, mg_str.as_bytes(), 0xFFA500);  // 橙色
                }
            }

            start_ticks = end_ticks;
        }

        // 5. 为下一轮做准备 (全是加法)
        my_next_target += n_cores;
        local_frame_idx += 1;
        if local_frame_idx >= ctx.total_frames {
            local_frame_idx  = 0;
        }
    }
}

pub fn mp_draw(
    screen: &mut Screen,
    file: &mut RegularFile,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    // 1 解码
    let mp_handle = get_handle_for_protocol::<MpServices>()?;
    let mp = open_protocol_exclusive::<MpServices>(mp_handle)?;
    let n_cores = mp.get_number_of_processors()?.enabled;
    let mut info_buf = vec![0u8; 128];
    let info = loop {
        match file.get_info::<FileInfo>(&mut info_buf) {
            Ok(info) => break info,
            Err(e) if e.status() == Status::BUFFER_TOO_SMALL => {
                if let Some(size) = *e.data() { info_buf.resize(size, 0) }
            }
            Err(e) => return Err(e.status())?,
        }
    };
    let mut compressed_buffer = vec![0u8; info.file_size() as usize];
    file.read(&mut compressed_buffer).map_err(|e| e.status())?;

    // 准备核心存储容器: [核心ID][帧ID] -> 这一帧该核负责的像素切片
    let mut core_frames: CoreFrameSegment = (0..n_cores).map(|_| Vec::new()).collect();
    let mut offset = 0;

    let (scr_width, scr_height) = screen.get_gop().current_mode_info().resolution();
    let rows_per_core = scr_height / n_cores;

    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr();
    let scr_stride = screen.get_gop().current_mode_info().stride();

    // 原始单帧空间初始化
    let mut single_raw: Frame = vec![0u8; width * height * 4];
    // 文件末尾越界保护
    while offset + 4 <= compressed_buffer.len() {
        let len_bytes = &compressed_buffer[offset..offset + 4];
        let frame_len =
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        let data_start = offset + 4;
        let next_frame_pos = data_start + frame_len;

        if next_frame_pos > compressed_buffer.len() { break }

        // 获取当前Qoi帧
        let frame_data = &compressed_buffer[data_start..next_frame_pos];

        if qoi::decode_to_buf(&mut single_raw, frame_data).is_err() {
            offset = next_frame_pos;
            continue
        }

        // 红蓝交换 (RGBA -> BGRA)
        for chunk in single_raw.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        // 核心切分
        for core_id in 0..n_cores {
            let y_start = core_id * rows_per_core; // 该核心负责的屏幕起始行
            let y_end = if core_id == n_cores - 1 { scr_height } else { y_start + rows_per_core };

            let mut frame: Frame = Vec::new();
            let stride_bytes = scr_stride * 4; // 屏幕物理跨度
            let row_bytes = width * 4;        // 视频实际宽度

            for y in y_start..y_end {
                let video_y = y;

                if video_y < height {
                    // 只有当屏幕行落在视频高度范围内时，才去 single_raw 拿数据
                    let src_row_start = video_y * width * 4;
                    let src_row_end = src_row_start + row_bytes;

                    if src_row_end > single_raw.len() {
                        Err(Status::INVALID_PARAMETER)?
                    }

                    // 1. 拷贝视频行
                    frame.extend_from_slice(&single_raw[src_row_start..src_row_end]);

                    // 2. 补齐当前行到屏幕的 stride 跨度
                    if stride_bytes > row_bytes {
                        let padding = stride_bytes - row_bytes;
                        frame.resize(frame.len() + padding, 0u8);
                    }
                } else {
                    // 如果屏幕行超过了视频高度，这一行全是黑的（但也要占满 stride 长度）
                    frame.resize(frame.len() + stride_bytes, 0u8);
                }
            }
            core_frames[core_id].push(frame);
        }
        offset = next_frame_pos;
    }

    // 2 构造参数
    let stride_bytes = scr_stride * 4;

    // 用来存储所有核心任务包的指针

    // --- 准备指针矩阵 ---
    // 我们需要把 Vec<Vec<Vec<u8>>> 转换成三级指针，方便 AP 访问
    let mut all_core_lists = Vec::new();
    for core_id in 0..n_cores {
        let frame_addrs: Vec<*const u8> = core_frames[core_id].iter().map(|f| f.as_ptr()).collect();
        all_core_lists.push(Box::leak(frame_addrs.into_boxed_slice()).as_ptr());
    }
    let core_frame_ptrs = Box::leak(all_core_lists.into_boxed_slice()).as_ptr();

    let (scr_width, scr_height) = screen.get_gop().current_mode_info().resolution();
    let scr_stride = screen.get_gop().current_mode_info().stride();
    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr();

    let sync_counter = Box::leak(Box::new(AtomicUsize::new(0)));

    // --- 构造统一 Context ---
    let ctx = Box::leak(Box::new(PlayTask {
        mp: &mp,
        fb_base,
        stride_bytes: scr_stride * 4,
        width,
        height: scr_height,
        num_cores: n_cores,
        core_frame_ptrs,
        total_frames: core_frames[0].len(),
        sync_counter,
        target_fps: target.unwrap_or(0),
        tps,
    }));

    let arg_ptr = ctx as *mut _ as *mut c_void;

    let event = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None)? };

    // --- 启动 AP ---
    if n_cores > 1 {
        // 使用你彩色代码中成功的 startup_all_aps
        // 注意：如果你需要它不阻塞持续播放，这里要设置为 false (非阻塞启动)
        let _ = mp.startup_all_aps(false, play_task, arg_ptr, Some(event), None);
    }

    // --- BSP 亲自执行 ---
    play_task(arg_ptr);

    Ok(())
}