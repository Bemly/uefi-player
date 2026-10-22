//! 统一控制面分工（解码并行）：`mp_draw` 恒 `clip`/`eager`（需全前视做行区切分，
//! `need_eager_clip` 强制，`single`/窗懒不进此路）；BSP 解码（load 期全量，播时
//! 只拷屏），AP 只拷行区（`copy_block` 位宽选路，永不解码有状态流、永不碰 GOP/Blt；
//! Blt 整推只 BSP）。解码-while-上传流水在 `blt`/`direct`/`nano` 懒侧（窗预取+环形
//! 多缓冲，见 `decoder::lazy`），本路以全量帧表换确定性（`tile`/`dynamic` 同）。
//!
//! zero-copy×mp 帧分区（`mp_slots` 子模块，本文件只分发）：格式 qois/raw
//! （帧独立，见 `decoder::source` 可行性头）+ 全核 + 有效供片 zero_copy/single/
//! stream（`workers=all` 快照）时，帧按核分区、各核解码进自家槽环（`SlotRing`），
//! BSP 按序整推且独判掉帧（确定性规则不变）；其余一律走下述行分区 eager 路
//! （逐字节旧行为）。新标记行（矩阵 follow-up 断言用）：
//! `play: path=mp_slots frame-parallel fmt=<qois|raw> fetch=<feed> cores=<n> slots=<n>`。
//!
//! dynamic 行边界发布经 `graphics::mp_copy::BoundGate`（`SeqGate`，替代旧
//! `bounds`+`bgen` 裸数组对；AP 读侧 tsc-bounded 重试，失败沿用旧行区）。
//! 校准会话（`mp_copy::MpDrive`）沿用 raw 数组，行为零改。
//!
//! `[display]` v1 视觉不变（仍 native 左上 + 黑垫）：行分区按屏行切、槽环按屏
//! stride 建，BSP 起播预缩放整帧表 + AP 分带重映射另立项（逐帧 8MB 暂存 +
//! 分区重算，须另测 OOM/带宽）；`display:` marker 照记（配置意图，见入口）。

use super::decoder::Clip;
use super::rdtsc;
use crate::config::{self, CopyWidth, DistMode, RolesMode};
use crate::error::Result;
use crate::fs::Fs;
use crate::graphics::mp_copy::{BoundGate, publish_bound_gate, read_bound_gate};
use crate::graphics::{Screen, copy_block, dynamic_row_bounds};
use crate::video::ascii_font::FONT_8X16;
use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::{format, vec};
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use uefi::boot::{EventType, Tpl, create_event, get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::pi::mp::MpServices;
use uefi::{Status, boot};

/// qois/raw 零拷×多核帧分区槽环路（本文件只分发，主体见该模块；`mp.rs` 保持 ≤1000 行）。
#[path = "mp_slots.rs"]
pub(crate) mod mp_slots;

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
    target_fps: u32,               // 0 = 不限速（max 模式）
    tps: u64,
    /// P2 位宽（`config::copy_width` 快照；AP/BSP 同值，见 `copy_block` 门控）。
    copy: u8,
    /// P2 调度（`config::effective_dist` 快照；tile 时 BSP+AP 从 `tile` 领活；
    /// dynamic 时 BSP+AP 按发布门加权分行，`seq` 做发布世代）。
    dist: u8,
    tile: &'a AtomicUsize,
    tile_rows: usize,
    tiles_total: usize,
    /// P4-dynamic 源：全帧指针表（`[total_frames]`，每帧行 stride 对齐；
    /// static/tile 沿用 `core_frame_ptrs` 切片，本字段仅 dynamic 有效）。
    dyn_frames: *const *const u8,
    /// P4-dynamic 行边界发布门（`mp_copy::BoundGate`，n+1 项行号，首项恒 `OSD_ROWS`，
    /// AP 不盖 OSD 字，与 tile 同策略）。BSP 独写发布、AP 无锁快照读
    /// （`read_bound_gate` tsc-bounded 重试，失败沿用旧行区）。
    seq: *const BoundGate,
    /// P4-dynamic 计时：每核本轮搬运行耗时（BSP 读来算下轮权重，EWMA 平滑）。
    btimes: *const AtomicUsize,
    /// 后缓冲影子槽（`tune.buffers > 0` 时）：`sh_n` 个槽指针（每槽
    /// `sh_h` 行 × `sh_row` 字节，屏几何 stride 对齐），AP/ BSP 画影子，
    /// BSP 栅栏后整推前台。0 = 直写前台（现状）；Blt 路不用（已是单 push）。
    sh_slots: *const *mut u8,
    sh_n: usize,
    sh_row: usize,
    sh_h: usize,
    /// 影子整推发布世代（BSP 推完 bump；AP 下轮开工前等它，N==1 也无竞态）。
    push_gen: *const AtomicUsize,
    /// GOP 裸指针（只给 BSP 调 Blt 整推用；AP 永不碰 GOP）。
    /// `mp_draw` 帧内不返回，Screen 活着，安全。
    gop: *mut GraphicsOutput,
    /// P3 掉帧指令（BSP 独写、AP 只读；`drop_gen` 是世代号，见循环注释）。
    drop_gen: &'a AtomicUsize,
    drop_flag: &'a AtomicUsize,
}

/// OSD 行数（顶条）：tile 搬运跳过这些行，AP 不再盖 OSD 字
///（静态分区遗留行为：AP 全区直盖，OSD 闪烁，见 `player.md`）。
pub const OSD_ROWS: usize = 20;

/// tile 行区搬运：逐行 `copy_block`（P2 位宽选路），OSD 行由调用方先钳掉。
/// edition 2024：unsafe fn 体内显式 unsafe 块。
pub(crate) unsafe fn copy_tile_rows(
    dst_base: *mut u8,
    dst_stride: usize,
    src_frame: *const u8,
    row_bytes: usize,
    y0: usize,
    y1: usize,
    w: CopyWidth,
) {
    unsafe {
        for y in y0..y1 {
            let d = dst_base.add(y * dst_stride);
            let s = src_frame.add(y * row_bytes);
            copy_block(d, s, row_bytes, w);
        }
    }
}

extern "efiapi" fn play_task(arg: *mut c_void) {
    if arg.is_null() {
        return;
    }
    let ctx = unsafe { &*(arg as *const PlayTask) };

    let my_id = unsafe { (*ctx.mp).who_am_i().unwrap_or(usize::MAX) };
    // 选中集门（`crew::ap_index_in_crew`）：编号 ≥ k 的闲 AP 早退（见 `crew`
    // 模块头 start-all-but-idle-unchosen），永不碰行区/GOP。
    if config::ap_index_in_crew(my_id, ctx.num_cores).is_none() {
        return;
    }

    // P4-dynamic 开关（快照自 `effective_dist`；static/tile/dynamic 三路互斥）。
    let dynamic = ctx.dist == DistMode::Dynamic as u8;
    // 静态切片参数（static/tile 路用；dynamic 走全帧源 + 加权边界，不碰它——
    // dynamic 下解码根本不切分片，`core_frame_ptrs` 为空，解引用会踩空指针）。
    let rows_per_core = ctx.height / ctx.num_cores;
    let y_start = my_id * rows_per_core;
    let (my_frames_list, my_block_size): (*const *const u8, usize) = if dynamic {
        (core::ptr::null(), 0)
    } else {
        unsafe {
            (
                *ctx.core_frame_ptrs.add(my_id),
                if my_id == ctx.num_cores - 1 {
                    (ctx.height - y_start) * ctx.stride_bytes
                } else {
                    rows_per_core * ctx.stride_bytes
                },
            )
        }
    };

    let mut local_frame_idx = 0;
    let n_cores = ctx.num_cores;
    // P2：本核搬运位宽（快照自 boot 时 `resolve_accel`，全核一致）。
    let copy_w = CopyWidth::from_u8(ctx.copy);
    // P4-dynamic 本核行区（首轮等分，见下；BSP 每轮发布后按快照装载）。
    // BSP 另有一张 EWMA 均值表（`avg`），每轮按各核耗时重算权重。
    // 影子整推发布世代（首轮无 push 可等，0 即就绪）。
    let mut local_push = 0usize;
    // 混合 Blt 整推失败只记一次 warning（随后轮次静默走 CPU 回退）。
    let mut push_blt_warned = false;
    let (mut dy0, mut dy1) = (OSD_ROWS, ctx.height);
    let mut avg: Vec<u64> = if dynamic && my_id == 0 {
        vec![0u64; n_cores.max(1)]
    } else {
        Vec::new()
    };
    // P3：掉帧迭代计数 + BSP 侧累计（确定性：全核读同一发布值，见下）。
    let mut iter: usize = 0;
    let mut drops_total: u64 = 0;
    // 初始化第一个目标值：第一帧写完时，计数器应该达到 n_cores
    let mut my_next_target = n_cores;

    // 计算帧率
    let ticks_per_sec;
    let target_ticks_per_frame;
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
    unsafe fn draw_string_opaque(
        fb_base: *mut u32,
        stride: usize,
        x: u32,
        y: u32,
        s: &[u8],
        color: u32,
    ) {
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
    let frame_row_bytes = ctx.width * 4;
    let ui_height = OSD_ROWS;
    let start_y = ui_height;
    let end_y = (ctx.height / 4) as usize;

    // 计算偏移量和拷贝大小（也是固定的）
    let offset = start_y * stride;
    let copy_size = if end_y > start_y {
        (end_y - start_y) * stride
    } else {
        0
    };
    loop {
        // 0. 取本轮掉帧指令（P3 确定性设计）：
        //    BSP 在上轮末尾发布 `(drop_gen = iter, drop_flag)`，AP 自旋等到可见再开工。
        //    首轮 gen 预置 0 即就绪。逐核按自家时钟判迟早会发散（栅栏 skew），
        //    所以只有 BSP 的判定算数，全核执行同一结论，索引永不发散。
        //    max 模式（target=0）不发布、固定不丢（flag 恒 0）。
        let mut skip = 0usize;
        if ctx.target_fps > 0 {
            while ctx.drop_gen.load(Ordering::Acquire) < iter {
                core::hint::spin_loop();
            }
            skip = ctx.drop_flag.load(Ordering::Acquire).min(1);
        }
        let fi = (local_frame_idx + skip) % ctx.total_frames.max(1);
        let fstart = rdtsc();
        // 后缓冲出屏基址：有影子时全核画本轮槽（slot 按轮次取模，人人同值），
        // BSP 栅栏后整推前台；无影子直写前台（现状，零改动）。
        let sh_slot: *mut u8 = if ctx.sh_n > 0 {
            unsafe { *ctx.sh_slots.add(iter % ctx.sh_n) }
        } else {
            core::ptr::null_mut()
        };
        let out_base = if sh_slot.is_null() {
            ctx.fb_base
        } else {
            sh_slot
        };
        // 1. 搬运 (生产)：static 按核分区；tile 从共享游标领活
        //    （`tile` 单调递增，`base = iter*N`，永不回绕；OSD 行跳过）；
        //    dynamic 按发布的加权边界分行（BSP 每轮按各核耗时重算，快核多拿，
        //    无游标原子开销；OSD 行恒划在边界外，AP 不盖字，与 tile 同策略）。
        let tiled = !dynamic && ctx.dist == DistMode::Tile as u8;
        if dynamic {
            // SeqGate 快照读（发布侧 `mp_draw` 建门 + BSP 独写；读撕裂则
            // `read_bound_gate` 内 tsc-bounded 重试，到界 false 即沿用旧行区——
            // 行区只影响本轮负载分布，不影响正确性；发布值不变时读回相同，
            // 与旧“世代不变不重载”等价，无需世代计数器）。
            let gate = unsafe { &*ctx.seq };
            let mut tmp = [0usize; 129];
            if read_bound_gate(gate, n_cores + 1, &mut tmp, ctx.tps) {
                dy0 = tmp[my_id].min(ctx.height);
                dy1 = tmp[my_id + 1].min(ctx.height).max(dy0);
            }
            let src_frame = unsafe { *ctx.dyn_frames.add(fi) };
            let t0 = rdtsc();
            let mut y = dy0;
            while y < dy1 {
                unsafe {
                    let d = out_base.add(y * ctx.stride_bytes);
                    let s = src_frame.add(y * ctx.stride_bytes);
                    copy_block(d, s, ctx.stride_bytes, copy_w);
                }
                y += 1;
            }
            unsafe {
                (*ctx.btimes.add(my_id))
                    .store(rdtsc().saturating_sub(t0) as usize, Ordering::Relaxed)
            };
        } else if tiled {
            let base = (iter as u64).saturating_mul(ctx.tiles_total as u64);
            let src_frame_base = unsafe { *my_frames_list.add(fi) as *const u8 };
            loop {
                let t = ctx.tile.fetch_add(1, Ordering::SeqCst) as u64;
                let rel = t.saturating_sub(base) as usize;
                if rel >= ctx.tiles_total {
                    break;
                }
                let mut ty0 = rel * ctx.tile_rows;
                let ty1 = (ty0 + ctx.tile_rows).min(ctx.height);
                if ty0 < OSD_ROWS {
                    ty0 = OSD_ROWS;
                }
                if ty0 >= ty1 {
                    continue;
                }
                unsafe {
                    copy_tile_rows(
                        out_base,
                        ctx.stride_bytes,
                        src_frame_base,
                        frame_row_bytes,
                        ty0,
                        ty1,
                        copy_w,
                    );
                }
            }
        } else {
            unsafe {
                if my_id == 0 {
                    // 1. 获取当前帧的源地址（这个不能移出去，因为每帧 index 不同）
                    let src_frame_base = unsafe { *my_frames_list.add(fi) as *const u32 };

                    // 2. 镂空搬运（P2 位宽选路，OSD 行区留白由 copy_size 保证；
                    //    有影子时画槽，BSP 栅栏后整推，见下）
                    if copy_size > 0 {
                        copy_block(
                            unsafe { (out_base as *mut u32).add(offset) as *mut u8 },
                            unsafe { src_frame_base.add(offset) as *const u8 },
                            copy_size,
                            copy_w,
                        );
                    }
                } else {
                    let src_ptr = *my_frames_list.add(fi);
                    let my_dst = out_base.add(y_start * ctx.stride_bytes);
                    copy_block(my_dst, src_ptr, my_block_size, copy_w);
                }
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

        // 3.2 dynamic 自适应（BSP 独写）：各核本轮耗时已齐（栅栏后可见），
        // EWMA 平滑后重算加权边界并发布新世代。AP 下轮按世代重载行区；
        // max 不限速时 AP 不等 drop_gen 也安全（旧世代配旧边界，恒成对）。
        if dynamic && my_id == 0 {
            for i in 0..n_cores {
                let t = unsafe { (*ctx.btimes.add(i)).load(Ordering::Relaxed) } as u64;
                avg[i] = if avg[i] == 0 { t } else { (avg[i] * 3 + t) / 4 };
            }
            let b = dynamic_row_bounds(&avg, n_cores, OSD_ROWS, ctx.height);
            // SeqGate 发布（BSP 独写；AP 下轮快照读即见新边界，
            // 撕裂读只影响本轮负载分布，不影响正确性）。
            publish_bound_gate(unsafe { &*ctx.seq }, &b);
            // BSP 自己的下一轮行区：刚发布的最热边界，直接装载。
            dy0 = b[my_id].min(ctx.height);
            dy1 = b[my_id + 1].min(ctx.height).max(dy0);
        }

        // 3.3 OSD/统计（BSP）：画进出屏基址——有影子时画槽，随整推上屏
        // （若直画前台，下帧整推会盖掉字；tile/dynamic 下 AP 不盖 OSD 行，
        // static 盖字是已知瑕疵，见 player.md）。
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

                let fb = out_base as *mut u32;
                // 注意：stride 必须是像素跨度，确保你在 loop 外已经算好了 stride = stride_bytes / 4

                // --- 绘制逻辑 ---
                let fps_str = format!("FPS: {:>4}", fps);
                let ft_str = format!("FT: {:>5} us", ft_us);
                let mg_str = format!("Margin: {:>5} us Drop:{}", margin_us, drops_total);

                unsafe {
                    // 并排显示在最顶层 (y=0)
                    draw_string_opaque(fb, stride, 0, 0, fps_str.as_bytes(), 0x00FF00); // 绿色
                    draw_string_opaque(fb, stride, 200, 0, ft_str.as_bytes(), 0x00FFFF); // 青色
                    draw_string_opaque(fb, stride, 450, 0, mg_str.as_bytes(), 0xFFA500); // 橙色
                }
            }

            start_ticks = end_ticks;
        }
        // 3.4 影子整推（BSP 独推）：栅栏后全核画完，BSP 把本轮槽整推前台并
        // 发布世代；AP 下轮开工前等它（N==1 也无竞态）。直写路（sh_n==0）跳过。
        // 混合路用 Blt 引擎整推（与矩阵 hybrid cell 同路径）；引擎失败退回
        // CPU 整推（同字节），只记一次 warning。
        if ctx.sh_n > 0 {
            if my_id == 0 {
                let px = ctx.sh_h.saturating_mul(ctx.sh_row) / 4;
                let slot = unsafe { *ctx.sh_slots.add(iter % ctx.sh_n) };
                let ok = unsafe {
                    let buf = core::slice::from_raw_parts(slot as *const BltPixel, px);
                    (*ctx.gop)
                        .blt(BltOp::BufferToVideo {
                            buffer: buf,
                            src: BltRegion::Full,
                            dest: (0, 0),
                            dims: (ctx.sh_row / 4, ctx.sh_h),
                        })
                        .is_ok()
                };
                if !ok {
                    if !push_blt_warned {
                        push_blt_warned = true;
                        log::warn!("play: hybrid Blt push failed, CPU fallback");
                    }
                    unsafe { copy_block(ctx.fb_base, slot, ctx.sh_h * ctx.sh_row, copy_w) };
                }
                local_push = local_push.wrapping_add(1);
                unsafe { (*ctx.push_gen).store(local_push, Ordering::SeqCst) };
            }
            while unsafe { (*ctx.push_gen).load(Ordering::Acquire) } < iter + 1 {
                core::hint::spin_loop();
            }
        }
        // 3.5 限速 + 下轮掉帧指令发布：栅栏刚对齐过，各核按同一预算自旋。
        // BSP 判迟（本轮耗时超 2 倍预算 → 下轮丢 1 帧追赶），独写发布；
        // AP 只等 gen 可见。BSP 不等（自己刚发布），AP 等（发布必达，无死锁：
        // BSP 发布前不依赖 AP，AP 等待前已过本轮栅栏）。
        if ctx.target_fps > 0 {
            let budget = ctx.tps.max(1) / ctx.target_fps.max(1) as u64;
            while rdtsc().saturating_sub(fstart) < budget {
                core::hint::spin_loop();
            }
            if my_id == 0 {
                let late = rdtsc().saturating_sub(fstart) > budget.saturating_mul(2);
                if late {
                    drops_total += 1;
                }
                ctx.drop_flag.store(late as usize, Ordering::SeqCst);
                ctx.drop_gen.store(iter + 1, Ordering::SeqCst);
            } else {
                while ctx.drop_gen.load(Ordering::Acquire) < iter + 1 {
                    core::hint::spin_loop();
                }
            }
        }

        // 5. 为下一轮做准备 (全是加法)
        my_next_target += n_cores;
        // fi 已含本轮 skip，下一起点顺延（全核同公式，永不发散）。
        local_frame_idx = (fi + 1) % ctx.total_frames.max(1);
        iter += 1;
        // 里程碑：BSP 每 256 轮报一次（串口-only；卡死时最后行号即定位）。
        if my_id == 0 && (iter & 255) == 0 {
            log::warn!("play: alive round={} drops={}", iter, drops_total);
        }
    }
}

pub fn mp_draw(
    screen: &mut Screen,
    mut clip: Clip,
    _width: usize,
    _height: usize,
    target: Option<u32>,
    tps: u64,
) -> Result {
    // 1 切片（行分区 eager 路：`Clip` 已预解码：qois 整文件解码 / raw 整目录直读，
    // 双格式同一下游切分；坏帧记数在 `Clip` 内，见 `decoder.rs`）。
    let mp_handle = get_handle_for_protocol::<MpServices>()?;
    let mp = open_protocol_exclusive::<MpServices>(mp_handle)?;
    // 选中集（`crew` 决议：BSP-id 自检 + 预算/记忆/workers 优先级；行分区、
    // BoundGate、影子环全按 k 算，闲 AP 由 `play_task` 入口早退）。
    let detected = mp.get_number_of_processors()?.enabled.max(1);
    let n_cores = config::resolve_crew_count(detected, mp.who_am_i().unwrap_or(0));
    // 槽路裁决（准入且 roles != combined；显式 split 不准入时内含一次 warn）；
    // 生效 roles 与裁决值恒一致（分即 Split，不分即 Combined），直接派生，
    // 不二次调 `effective_roles`（免重复 warn）。
    let slots = mp_slots::use_frame_slots(n_cores);
    let roles = if slots {
        RolesMode::Split
    } else {
        RolesMode::Combined
    };
    config::emit_sched_line(n_cores, detected, roles);
    // zero-copy×mp 帧分区：qois/raw 无状态帧并行 + 准入 + 槽路角色 +
    // lazy 编码源就绪时，帧按核分区、各核真解码进槽环、BSP 按序整推（见 `mp_slots`）；
    // eager Clip 走下述行分区 eager 路（防双解码浪费）；lazy 残留（单核/配置漂移）
    // 就地补全，确保下述行分区恒吃 eager（逐字节旧行为）。
    if slots && clip.lazy.is_some() {
        return mp_slots::mp_draw_slots(screen, clip, target, tps, &mp);
    }
    clip.ensure_eager()?;
    {
        let (dw, dh) = screen.get_gop().current_mode_info().resolution();
        crate::graphics::present::note_clip_start(
            clip.w as u32,
            clip.h as u32,
            dw as u32,
            dh as u32,
        );
    }
    // 片源几何（qois 取文件头一致值，raw 取定值 1920x1080）。
    let (width, height) = (clip.w, clip.h);

    // 准备核心存储容器: [核心ID][帧ID] -> 这一帧该核负责的像素切片
    // （dynamic 不切分片，见下全帧表；容器保持空，`core_frame_ptrs` 传空）。
    let mut core_frames: CoreFrameSegment = (0..n_cores).map(|_| Vec::new()).collect();
    // P4-dynamic 全帧表：每帧 scr_height 行 × stride（行区由 bounds 逐轮重划，
    // 源必须是全帧，与 static/tile 的等分切片不兼容，内存总量同级）。
    let mut dyn_frames: Vec<Frame> = Vec::new();

    let (scr_width, scr_height) = screen.get_gop().current_mode_info().resolution();
    let rows_per_core = scr_height / n_cores;

    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr();
    let scr_stride = screen.get_gop().current_mode_info().stride();
    let stride_bytes = scr_stride * 4; // 屏幕物理跨度
    let row_bytes = width * 4; // 视频实际宽度
    // P4-dynamic 开关（`video_run` 只在 direct+all 时进 `mp_draw`，
    // 此时 `effective_dist` 的 direct+all 短路已过，本值即真实调度；
    // `BoundGate` 定长 129（≤128 核），超限回落 static 并记 warn，不断播）。
    let mut dynamic = config::effective_dist() == DistMode::Dynamic;
    if dynamic && n_cores > crate::graphics::mp_copy::BOUND_GATE_MAX_CORES {
        log::warn!(
            "play: dynamic cores {} exceed bound gate, fallback static",
            n_cores
        );
        dynamic = false;
    }

    // `Clip` 帧已是 BGRA `BltPixel`：reinterpret 为字节做行切分（零拷贝视图，
    // 下游 `copy_block` 按字节搬运，与旧 `single_raw` 同布局）。
    for frame in &clip.frames {
        let raw: &[u8] =
            unsafe { core::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) };
        if raw.len() < height.saturating_mul(row_bytes) {
            continue;
        }

        // dynamic 全帧：视频行拷入 + stride 补齐，超高行置黑（与切片同语义，
        // 行区划分推迟到播放时按 bounds 逐轮重算）。
        if dynamic {
            let mut full: Frame = vec![0u8; scr_height * stride_bytes];
            for y in 0..scr_height {
                if y < height {
                    let s0 = y * width * 4;
                    let s1 = s0 + row_bytes;
                    if s1 > raw.len() {
                        Err(Status::INVALID_PARAMETER)?
                    }
                    full[y * stride_bytes..y * stride_bytes + row_bytes]
                        .copy_from_slice(&raw[s0..s1]);
                }
            }
            dyn_frames.push(full);
        } else {
            // 核心切分（static/tile 路，不动）
            for core_id in 0..n_cores {
                let y_start = core_id * rows_per_core; // 该核心负责的屏幕起始行
                let y_end = if core_id == n_cores - 1 {
                    scr_height
                } else {
                    y_start + rows_per_core
                };

                let mut frame: Frame = Vec::new();

                for y in y_start..y_end {
                    let video_y = y;

                    if video_y < height {
                        // 只有当屏幕行落在视频高度范围内时，才去片源拿数据
                        let src_row_start = video_y * width * 4;
                        let src_row_end = src_row_start + row_bytes;

                        if src_row_end > raw.len() {
                            Err(Status::INVALID_PARAMETER)?
                        }

                        // 1. 拷贝视频行
                        frame.extend_from_slice(&raw[src_row_start..src_row_end]);

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
        }
    }

    // 空集守卫：全坏文件直接报错，不进“AP 读空指针 + 无限循环”（曾 OOB）。
    // `Clip` 构造时已保证非空（空集直接 Err），这里是双保险。
    let total_frames = if dynamic {
        dyn_frames.len()
    } else {
        core_frames[0].len()
    };
    if total_frames == 0 {
        return Err(crate::error::NyaStatus::_Debug(alloc::format!(
            "video: no valid frames"
        )));
    }

    // 2 构造参数

    // 用来存储所有核心任务包的指针

    // --- 准备指针矩阵（static/tile 用；dynamic 传空，AP 不碰，见 play_task） ---
    let core_frame_ptrs: *const *const *const u8 = if dynamic {
        core::ptr::null()
    } else {
        // 我们需要把 Vec<Vec<Vec<u8>>> 转换成三级指针，方便 AP 访问
        let mut all_core_lists = Vec::new();
        for core_id in 0..n_cores {
            let frame_addrs: Vec<*const u8> =
                core_frames[core_id].iter().map(|f| f.as_ptr()).collect();
            all_core_lists.push(Box::leak(frame_addrs.into_boxed_slice()).as_ptr());
        }
        Box::leak(all_core_lists.into_boxed_slice()).as_ptr()
    };

    // --- P4-dynamic 共享状态（Box 漏存，会话期常驻；播完不回收，同既有风格） ---
    // 行边界经 `BoundGate` 发布（替代旧 `bounds`+`bgen` 裸数组对；校准侧不动）。
    let (dyn_frames_ptr, seq_ptr, btimes): (
        *const *const u8,
        *const BoundGate,
        *const AtomicUsize,
    ) = if dynamic {
        let frame_addrs: Vec<*const u8> = dyn_frames.iter().map(|f| f.as_ptr()).collect();
        let fp = Box::leak(frame_addrs.into_boxed_slice()).as_ptr();
        // 首轮等分 `[OSD_ROWS, scr_height)`；BSP 首轮栅栏后即按实测重算发布。
        let init = dynamic_row_bounds(&vec![0u64; n_cores.max(1)], n_cores, OSD_ROWS, scr_height);
        let gate: &'static BoundGate = Box::leak(Box::new(BoundGate::new()));
        publish_bound_gate(gate, &init);
        log::info!("mp: bound SeqGate armed n={}", n_cores);
        let mut tv = Vec::with_capacity(n_cores.max(1));
        for _ in 0..n_cores.max(1) {
            tv.push(AtomicUsize::new(0));
        }
        let tp: *const AtomicUsize = Box::leak(tv.into_boxed_slice()).as_ptr();
        (fp, gate, tp)
    } else {
        (core::ptr::null(), core::ptr::null(), core::ptr::null())
    };

    let (scr_width, scr_height) = screen.get_gop().current_mode_info().resolution();
    let scr_stride = screen.get_gop().current_mode_info().stride();
    let fb_base = screen.get_gop().frame_buffer().as_mut_ptr();
    // GOP 裸指针只给 BSP 调 Blt 整推用（AP 永不碰 GOP）。
    // `mp_draw` 帧内不返回（`play_task` 死循环），Screen 活着，安全。
    let gop_ptr = &mut **screen.get_gop() as *mut GraphicsOutput;

    let sync_counter = Box::leak(Box::new(AtomicUsize::new(0)));
    // P3 掉帧世代：gen 预置 0（首轮即就绪），flag 预置 0（不丢）。
    let drop_gen = Box::leak(Box::new(AtomicUsize::new(0)));
    let drop_flag = Box::leak(Box::new(AtomicUsize::new(0)));
    // P2 tile 游标：单调递增，`base = iter * tiles_total`，永不回绕。
    let tile = Box::leak(Box::new(AtomicUsize::new(0)));

    // --- 后缓冲影子环（`tune.buffers > 0` 时；屏几何 stride 对齐槽） ---
    // 分配失败（try_reserve）退回直写，只记 warning，不断播。
    // Blt 路不用影子（已是单引擎 push），`video_run` 只在 direct+all 时进这里。
    let mut sh_n = config::tune_buffers().min(16) as usize;
    let (sh_slots_ptr, sh_row, sh_h): (*const *mut u8, usize, usize) = if sh_n == 0 {
        (core::ptr::null(), 0, 0)
    } else {
        let slot_len = scr_height.saturating_mul(stride_bytes);
        let mut ring: Vec<Vec<u8>> = Vec::new();
        let mut ok = ring.try_reserve_exact(sh_n).is_ok() && slot_len > 0;
        if ok {
            for _ in 0..sh_n {
                let mut s: Vec<u8> = Vec::new();
                // 容量已预留，extend 不再分配，不会 panic。
                if s.try_reserve_exact(slot_len).is_err() {
                    ok = false;
                    break;
                }
                s.extend(core::iter::repeat(0u8).take(slot_len));
                ring.push(s);
            }
            ok = ok && ring.len() == sh_n;
        }
        if !ok {
            log::warn!(
                "video: shadow ring alloc failed ({} bufs), fallback direct",
                sh_n
            );
            sh_n = 0;
            (core::ptr::null(), 0, 0)
        } else {
            let leaked: &'static mut [Vec<u8>] = Box::leak(ring.into_boxed_slice());
            let mut ptrs: Vec<*mut u8> = Vec::new();
            for s in leaked.iter_mut() {
                ptrs.push(s.as_mut_ptr());
            }
            let p = Box::leak(ptrs.into_boxed_slice()).as_ptr();
            (p, stride_bytes, scr_height)
        }
    };
    // 影子整推发布世代：预置 0（首轮无 push 可等，即就绪）。
    let push_gen = Box::leak(Box::new(AtomicUsize::new(0)));

    // --- 构造统一 Context ---
    let ctx = Box::leak(Box::new(PlayTask {
        mp: &mp,
        fb_base,
        stride_bytes: scr_stride * 4,
        width,
        height: scr_height,
        num_cores: n_cores,
        core_frame_ptrs,
        total_frames,
        sync_counter,
        target_fps: target.unwrap_or(0),
        tps,
        copy: config::copy_width() as u8,
        dist: config::effective_dist() as u8,
        tile,
        tile_rows: config::tune_tile_rows() as usize,
        tiles_total: scr_height.div_ceil(config::tune_tile_rows().max(1) as usize),
        drop_gen,
        drop_flag,
        dyn_frames: dyn_frames_ptr,
        seq: seq_ptr,
        btimes,
        sh_slots: sh_slots_ptr,
        sh_n,
        sh_row,
        sh_h,
        push_gen,
        gop: gop_ptr,
    }));

    let arg_ptr = ctx as *mut _ as *mut c_void;

    let event = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None)? };

    // --- 启动 AP ---
    if n_cores > 1 {
        // 使用你彩色代码中成功的 startup_all_aps
        // 注意：如果你需要它不阻塞持续播放，这里要设置为 false (非阻塞启动)
        // 启动失败不静默：BSP 会永远卡在首轮栅栏（QEMU 实测定位过一次）。
        if let Err(e) = mp.startup_all_aps(false, play_task, arg_ptr, Some(event), None) {
            log::warn!(
                "play: startup_all_aps failed: {:?}, BSP-only fallback",
                e.status()
            );
        }
    }

    // --- BSP 亲自执行 ---
    play_task(arg_ptr);

    Ok(())
}
