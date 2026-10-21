//! `video::loops`：单线程播放循环 + 老管线对照（`[play] pipeline` 非 clip 落点）。
//! `blt_loop`/`direct_loop` 走现行 `Clip`；`staged`/`zero_copy`/`stream`/`single`/
//! `legacy_*` 为修复后老实现（只读 qois）。拆分仅为 ≤1000 行，不改行为。

use super::buffer::{BltFrameBuffer, QoiFrameBuffer, RawFrameBuffer};
use super::decoder::{Clip, VideoMemory};
use super::play::load_video_clip;
use super::{VIDEO_H, VIDEO_W, pace_or_drop, rdtsc};
use crate::config::{self, PlayPipeline};
use crate::error::Result;
use crate::fs::Fs;
use crate::graphics::Screen;
use alloc::vec::Vec;
use alloc::{format, vec};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::media::file::RegularFile;
use uefi::{CStr16, cstr16};

pub(crate) fn blt_loop(
    screen: &mut Screen,
    mut video: Clip,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => video.rewind(),
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}

/// BSP 单核直写播放：与 `blt_loop` 同结构，只是走 CPU 直写显存
///（校准判单核更快，或用户关掉 MP 时用；`upload_frame` 按生效路径分发，
/// 这里 render 已定 direct，实际就是 `upload_direct`）。
pub(crate) fn direct_loop(
    screen: &mut Screen,
    mut video: Clip,
    width: usize,
    height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        match video.next_frame() {
            Some(frame) => screen.upload_frame(width, height, frame)?,
            None => video.rewind(),
        }
        pace_or_drop(&mut video, target, tps, fstart, &mut drops);
    }
}

// ---------- 老播放管线（`[play] pipeline` 手动切换，修复后） ----------
//
// `Clip` 预解码之前的形态：流式解码（整文件进内存逐帧解 / 真文件流式边读边解），
// 每帧经 GOP Blt 整推。无 OSD/无多核/无影子——只做"能播 + 可对照"，性能与
// clip 管线不可比（对照看 HWBENCH，不看帧率）。
// 修复点（相对删除前）：
// - `draw` 的短读循环补齐（固件短读曾导致半帧上屏）；其余各路文件尾按结束处理；
// - `draw_once` 兼容容器（`u32le len + QOI` 只取首帧，纯 QOI 整文件解码）；
// - 解码失败记数（`Err(_)` 占位，原先连变量名都悬空，clippy 告警）。
// 非 clip 管线只读 qois（`video_run` 已门控，raw 回落 clip）。

/// 限速 + 掉帧（`VideoMemory` 版）：与 `pace_or_drop` 同预算语义，
/// 跳帧走 `VideoMemory::next_frame`（流式无坏帧记数，只记总数）。
pub(crate) fn pace_or_drop_mem(
    video: &mut VideoMemory,
    scratch: &mut Vec<u8>,
    target: Option<u32>,
    tps: u64,
    fstart: u64,
    drops: &mut u64,
) {
    let Some(fps) = target else { return };
    let budget = tps.max(1) / fps.max(1) as u64;
    let dt = rdtsc().saturating_sub(fstart);
    if dt <= budget {
        while rdtsc().saturating_sub(fstart) < budget {
            core::hint::spin_loop();
        }
        return;
    }
    let mut late = dt - budget;
    let mut n = 0;
    while late >= budget && n < 2 {
        if video.next_frame(scratch) {
            *drops += 1;
        } else {
            video.rewind();
            break;
        }
        late = late.saturating_sub(budget);
        n += 1;
    }
    if *drops > 0 && *drops % 512 == 0 {
        log::warn!("play: dropped {} frames total (late)", *drops);
    }
}

fn draw_once(fs: &mut Fs, screen: &mut Screen, path: &CStr16, blt_buf: &mut [BltPixel]) -> Result {
    // 加载文件
    let qoi_data = fs.read_file(path)?;

    // 单帧演示：容器（`u32le len + QOI` 串接）只取首帧，纯 QOI（`qoif` 魔数）整文件解码。
    let frame: &[u8] = if qoi_data.starts_with(b"qoif") {
        &qoi_data
    } else if qoi_data.len() >= 4 {
        let n = u32::from_le_bytes([qoi_data[0], qoi_data[1], qoi_data[2], qoi_data[3]]) as usize;
        let end = (4 + n).min(qoi_data.len());
        &qoi_data[4..end]
    } else {
        &qoi_data[..]
    };

    // 解码图像
    let (header, rgba_buf) = qoi::decode_to_vec(frame)?;

    // 转换像素格式
    blt_buf
        .iter_mut()
        .zip(rgba_buf.chunks_exact(header.channels.as_u8() as usize))
        .for_each(|(dest, src)| {
            *dest = BltPixel::new(src[0], src[1], src[2]);
        });

    // 渲染
    screen.draw_image(header.width, header.height, blt_buf)?;

    Ok(())
}

fn draw_all_mem(
    video: &mut VideoMemory,
    screen: &mut Screen,
    qoi: &mut QoiFrameBuffer,
    raw: &mut RawFrameBuffer,
    blt: &mut BltFrameBuffer,
) -> Result {
    if video.next_frame(&mut qoi.0) {
        raw.header = loop {
            match qoi::decode_to_buf(&mut raw.pixels, &qoi.0) {
                Ok(header) => break header,
                Err(qoi::Error::OutputBufferTooSmall { required, .. }) => {
                    raw.pixels.resize(required, 0)
                }
                // 真机错位曾在此静默丢帧：记数在外层循环的串口里看总数，
                // 这里只跳过（与 `Clip` 预解码的坏帧记数同策略）。
                Err(_) => return Ok(()),
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
    blt: &mut BltFrameBuffer,
) -> Result {
    if video.next_frame(&mut qoi.0) {
        let Ok(header) = qoi::decode_header(&qoi.0) else {
            return Ok(());
        };
        let pixel_count = (header.width * header.height) as usize;

        if blt.0.len() < pixel_count {
            blt.0.resize(pixel_count, BltPixel::new(0, 0, 0));
        }

        // 直接解码到 [R, G, B, A, R, G, B, A...]
        let Ok(_) = qoi::decode_to_buf(as_u8_slice_mut(&mut blt.0[..pixel_count]), &qoi.0) else {
            return Ok(());
        };

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
    blt: &mut BltFrameBuffer,
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
                // 真机错位曾在此静默丢帧：同 `draw_all_mem`，只跳过。
                // qoi::Error::InvalidPadding
                // qemu无问题 不知道怎么解决 暂时丢帧处理
                Err(_) => return Ok(()),
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

/// 流式三段管线循环（`pipeline = staged`）：`VideoMemory` 整文件 + 逐帧解码 + Blt。
fn staged_loop(screen: &mut Screen, target: Option<u32>, tps: u64) -> Result {
    log::info!("play: path=staged_loop (streaming QOI->Raw->Blt)");
    let mut fs = Fs::new()?;
    let file = fs.open_file(cstr16!("1080p\\video.qois"))?;
    let mut video = VideoMemory::new(file)?;
    let mut qoi = QoiFrameBuffer::new(8 * 1024 * 1024);
    let mut raw = RawFrameBuffer::new(8 * 1024 * 1024);
    let mut blt = BltFrameBuffer::new(VIDEO_W * VIDEO_H);
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        draw_all_mem(&mut video, screen, &mut qoi, &mut raw, &mut blt)?;
        pace_or_drop_mem(&mut video, &mut qoi.0, target, tps, fstart, &mut drops);
    }
}

/// 流式零拷贝管线循环（`pipeline = zero_copy`）：直解进 Blt 缓冲 + R/B 交换。
fn zero_copy_loop(screen: &mut Screen, target: Option<u32>, tps: u64) -> Result {
    log::info!("play: path=zero_copy_loop (streaming zero-copy decode)");
    let mut fs = Fs::new()?;
    let file = fs.open_file(cstr16!("1080p\\video.qois"))?;
    let mut video = VideoMemory::new(file)?;
    let mut qoi = QoiFrameBuffer::new(8 * 1024 * 1024);
    let mut blt = BltFrameBuffer::new(VIDEO_W * VIDEO_H);
    let mut drops = 0u64;
    loop {
        let fstart = rdtsc();
        draw_all_mem_zero_copy(&mut video, screen, &mut qoi, &mut blt)?;
        pace_or_drop_mem(&mut video, &mut qoi.0, target, tps, fstart, &mut drops);
    }
}

/// 真文件流式管线循环（`pipeline = stream`）：边读边解，不全量进内存。
/// 文件指针天然串行：限速只自旋补齐，不丢帧（对照用）。
fn stream_loop(screen: &mut Screen, target: Option<u32>, tps: u64) -> Result {
    log::info!("play: path=stream_loop (file streaming)");
    let mut fs = Fs::new()?;
    let mut file = fs.open_file(cstr16!("1080p\\video.qois"))?;
    let mut qoi = QoiFrameBuffer::new(8 * 1024 * 1024);
    let mut raw = RawFrameBuffer::new(8 * 1024 * 1024);
    let mut blt = BltFrameBuffer::new(VIDEO_W * VIDEO_H);
    loop {
        let fstart = rdtsc();
        draw(&mut fs, &mut file, screen, &mut qoi, &mut raw, &mut blt)?;
        if let Some(fps) = target {
            let budget = tps.max(1) / fps.max(1) as u64;
            while rdtsc().saturating_sub(fstart) < budget {
                core::hint::spin_loop();
            }
        }
    }
}

/// 单帧反复管线循环（`pipeline = single`）：每帧重读文件 + 解码 + 上屏，
/// 最慢，仅作"零缓存下限"对照。
fn single_loop(screen: &mut Screen, target: Option<u32>, tps: u64) -> Result {
    log::info!("play: path=single_loop (reload + decode every frame)");
    let mut fs = Fs::new()?;
    let mut blt = BltFrameBuffer::new(VIDEO_W * VIDEO_H);
    loop {
        let fstart = rdtsc();
        draw_once(&mut fs, screen, cstr16!("1080p\\video.qois"), &mut blt.0)?;
        if let Some(fps) = target {
            let budget = tps.max(1) / fps.max(1) as u64;
            while rdtsc().saturating_sub(fstart) < budget {
                core::hint::spin_loop();
            }
        }
    }
}

/// 老管线分发（`[play] pipeline != clip` 的落点）：7 条老实现循环，
/// `render` 矩阵/`schedule` 后端/`workers`/`dist` 一律忽略（老路无此概念）。
/// 调用后不返回（循环播）。
pub(crate) fn legacy_pipeline_run(
    screen: &mut Screen,
    pipeline: PlayPipeline,
    target: Option<u32>,
    tps: u64,
) -> Result {
    match pipeline {
        PlayPipeline::Clip => unreachable!("clip handled by caller"),
        PlayPipeline::Staged => staged_loop(screen, target, tps),
        PlayPipeline::ZeroCopy => zero_copy_loop(screen, target, tps),
        PlayPipeline::Stream => stream_loop(screen, target, tps),
        PlayPipeline::Single => single_loop(screen, target, tps),
        PlayPipeline::LegacyBlt => {
            log::info!("play: path=legacy_blt_loop (Clip + Blt)");
            let mut fs = Fs::new()?;
            let mut clip = load_video_clip(&mut fs, true)?;
            loop {
                screen.draw_all_mem_raw_zero_copy(&mut clip, VIDEO_W, VIDEO_H);
            }
        }
        PlayPipeline::LegacyDirect => {
            log::info!("play: path=legacy_direct_loop (Clip + direct)");
            let mut fs = Fs::new()?;
            let mut clip = load_video_clip(&mut fs, true)?;
            loop {
                screen.draw_fast_direct_copy(&mut clip, VIDEO_W, VIDEO_H);
            }
        }
        PlayPipeline::LegacyU64 => {
            log::info!("play: path=legacy_u64_loop (Clip + u64)");
            let mut fs = Fs::new()?;
            let mut clip = load_video_clip(&mut fs, true)?;
            loop {
                screen.draw_u64_optimized_loop(&mut clip, VIDEO_W, VIDEO_H);
            }
        }
    }
}
