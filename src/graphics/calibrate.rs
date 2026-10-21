//! `graphics::calibrate`：`render.mode/workers/dist/vsync = "auto"` 的开机矩阵校准。
//! 由 `Screen::ensure_render_calibrated` 落点（首启必跑全矩阵，有记忆复用快启）。
//! 拆分仅为满足单文件 ≤1000 行，不改行为。
//!
//! 统一控制面：校准只覆盖渲染/调度/vsync 维（解码表不进 bench）；AP 只拷校准行
//! （永不解码、永不碰 GOP，Blt 整推只 BSP，混合影子同 `mp` 纪律）。

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use core::time::Duration;

use uefi::boot::{
    EventType, Tpl, create_event, get_handle_for_protocol, open_protocol_exclusive, stall,
};
use uefi::proto::console::gop::{BltPixel, GraphicsOutput};
use uefi::proto::pi::mp::MpServices;

use crate::config::{self, BltOpKind, DistMode, RenderMode, VsyncMode, WorkersMode};

use super::mp_copy::{
    CAL_QUIT, MpCalCtx, MpDrive, ap_cal_copy, calibrate_tps, dynamic_row_bounds, rows_for_cal, tsc,
};

impl super::Screen {
    /// `render.mode/workers/dist/vsync = "auto"` 的开机矩阵校准（`\player.toml` 驱动）。
    ///
    /// - 首启（任一 auto 无记忆，见 `config::needs_render_bench`）必跑全矩阵：
    ///   直写 vs Blt 取快；直写路再比 BSP 单核 vs 全核并行，全核路再比
    ///   静态分区 vs tile 自调度 vs dynamic 加权（三选一）；auto 冠军路按预算跑一遍，
    ///   零超期则 vsync 开否则关。
    /// - 全核 cell 用 AP 常驻循环 + 轮次同步（`gop_bench` 方法论：单发 startup
    ///   测到的是 SIPI 唤醒开销而非扩展性，必须常驻 + 热身吸收后再计时）。
    /// - 首启 auto 追加核数 stage（仅 workers==auto + 无预算 + 无记忆 + 直写冠军）：
    ///   在冠军 dist 路上跑 `{1, half, all}` 三格（1/全复用主矩阵计时，仅 half 开
    ///   crew 新会话），5% 裁决（`config::pick_core_count`），回写
    ///   `[schedule] measured_workers_n`（三态同步写，只做展示兼容）。
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
        let mut pixels = vec![BltPixel::new(0x11, 0x22, 0x33); w * h];

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
        // 混合整推影子（`tune.buffers > 0` 且全核路在测时）：1 槽 stride 对齐，
        // AP/BSP 画影子，BSP 每轮 Blt 整推。与 Screen 影子环同布局（h×stride 连续）。
        // 分配失败退回直写（bench 照跑，只记 warning）。
        let fb_stride_px = self.gop.current_mode_info().stride();
        let want_hybrid = config::tune_buffers() > 0 && want_mp;
        let mut cal_shadow: Vec<BltPixel> = Vec::new();
        let need_px = h.saturating_mul(fb_stride_px);
        if want_hybrid && need_px > 0 && cal_shadow.try_reserve_exact(need_px).is_ok() {
            cal_shadow.extend(core::iter::repeat(BltPixel::new(0, 0, 0)).take(need_px));
        }
        let cal_sh: *mut u8 = if want_hybrid && cal_shadow.len() == need_px && need_px > 0 {
            cal_shadow.as_mut_ptr() as *mut u8
        } else {
            if want_hybrid {
                log::warn!("render auto: hybrid shadow alloc failed, fallback direct");
            }
            core::ptr::null_mut()
        };
        // GOP 裸指针只给 BSP 调 Blt 整推用（AP 永不碰 GOP）。Screen 活到函数尾，安全。
        let gop_ptr = &mut *self.gop as *mut GraphicsOutput;
        let mut mp_drive: Option<MpDrive> = None;
        if want_mp {
            if let Some(mp) = mp_store.as_deref() {
                let fb_base = self.gop.frame_buffer().as_mut_ptr();
                let fb_stride = self.gop.current_mode_info().stride() * 4;
                // 混合模式下 AP 画影子（`fb_base` 指向影子槽），BSP 每轮 Blt 整推；
                // 直写时沿用真前台，行为与旧版一致。
                let draw_base = if cal_sh.is_null() { fb_base } else { cal_sh };
                let ctx = MpCalCtx {
                    mp,
                    fb_base: draw_base,
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
                    // 全员参加（`cap == n`）；核数 stage 另建 crew 会话（见下）。
                    cap: n_cores,
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
                                fb_base: draw_base,
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
                                real_fb: fb_base,
                                gop: gop_ptr,
                                blt_push: !cal_sh.is_null(),
                                push_warned: false,
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

        // 各热身一次（单核路走生效路径，缓冲时含整推，与播放同路；
        // 多核路热身在会话计时里做）。
        let _ = self.upload_frame(w, h, &pixels);
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
        // Blt 引擎对照操作：所选 op 单发计时出数（供机型对比）。
        // 只有 BufferToVideo 能上屏播视频，其余只出数、不参选（见 player.md）；
        // 选中 B2V 时本格与 blt present 格同一操作，不重复测。
        let sel_op = config::blt_op();
        let mut acc_op = 0u64;
        let time_op = time_blt && sel_op != BltOpKind::BufferToVideo;
        if time_op {
            let _ = self.blt_op_once(sel_op, w, h, &mut pixels);
            for _ in 0..frames {
                let t = tsc();
                let ok = self.blt_op_once(sel_op, w, h, &mut pixels).is_ok();
                let dt = tsc().saturating_sub(t);
                if ok {
                    acc_op += dt;
                } else {
                    acc_op = 0;
                    break;
                }
            }
        }
        for _ in 0..frames {
            if time_direct {
                let t = tsc();
                // 单核路走生效路径（含缓冲整推，与播放同路，冠军可比）。
                let _ = self.upload_frame(w, h, &pixels);
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
        // 注：核数 stage（见下）在首启 auto 时可能把它从 All 翻回 BspOnly
        // （5% 口径下单核更快），故 `mut`；`persist_*` 在 stage 后统一算。
        let mut winner_workers = if !direct_in_play {
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
                // 三个有效 cell 取计时最小（全无效退回 tile 无害默认）。
                [
                    (static_valid, acc_static, DistMode::Static),
                    (tile_valid, acc_tile, DistMode::Tile),
                    (dynamic_valid, acc_dynamic, DistMode::Dynamic),
                ]
                .into_iter()
                .filter(|(valid, _, _)| *valid)
                .min_by_key(|(_, ticks, _)| *ticks)
                .map(|(_, _, dist)| dist)
                .unwrap_or(DistMode::Tile)
            } else {
                fixed_dist.unwrap_or(DistMode::Tile)
            };
        // ---- vsync 探测（auto）：冠军路按 `tune.vsync_fps` 预算跑 `frames` 帧，
        // 零超期才开（封顶同预算），否则关 ----
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
                        // 单核冠军走生效路径（含缓冲整推，与播放同路）。
                        let _ = self.upload_frame(w, h, &pixels);
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
                    VsyncMode::Static
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

        // ---- half 定档（显式 half 且无记忆）：冠军 cell 的矩阵计时即性能检测，
        // 零额外耗时——max_fps = frames * tps / 冠军耗时，取半稳定跑并回写。
        if config::vsync_mode() == VsyncMode::Half && config::measured_vsync_fps() == 0 {
            let wt = if winner_render == RenderMode::Blt {
                if time_blt { acc_blt } else { 0 }
            } else if winner_workers != WorkersMode::All {
                if time_direct { acc_direct } else { 0 }
            } else if winner_dist == DistMode::Static {
                acc_static
            } else if winner_dist == DistMode::Tile {
                acc_tile
            } else {
                acc_dynamic
            };
            if wt > 0 {
                let max_fps = (frames.saturating_mul(tps.max(1)) / wt.max(1)).clamp(1, 240) as u32;
                let half = (max_fps / 2).max(1);
                log::info!(
                    "render auto: half probe winner_ticks={} -> max {}fps, budget {}fps",
                    wt,
                    max_fps,
                    half
                );
                if let Err(e) = config::persist_measured_vsync_fps(half) {
                    log::warn!("render auto: half persist failed: {:?}", e);
                }
            } else {
                // 冠军 cell 无计时（理论到不了）：退回滑杆值，不断 boot。
                config::set_resolved_vsync_fps(config::tune_vsync_fps());
            }
        }

        // AP 退场（发 QUIT 后给一个循环周期退出，免得 Box 泄漏的 ctx 被踩）。
        cal_round.store(CAL_QUIT, Ordering::SeqCst);
        drop(mp_drive);
        stall(Duration::from_millis(50));

        // ---- 核数 stage（首启 auto sweep 专属：workers==auto + 无预算 +
        // 无记忆 + 直写冠军；只在冠军 dist 路上跑 {1, half, all} 三格——1 核格/
        // 全核格复用主矩阵计时，仅 half 格需新会话实测；5% 裁决，见 `crew` 头；
        // vsync 探测已跑完（仍用全核 drive，half 占优时偏乐观一档，见文件头）。
        // Blt 冠军不记（workers 无意义，与三态口径一致，避免后患）。
        let mut core_n = n_cores;
        let mem_eligible = probe_workers
            && config::schedule_cores() == 0
            && config::measured_workers_n() == 0
            && winner_render == RenderMode::Direct;
        if mem_eligible {
            let bsp = mp_store
                .as_deref()
                .and_then(|m| m.who_am_i().ok())
                .unwrap_or(0);
            if n_cores <= 1 || winner_workers != WorkersMode::All {
                // 单核机 / 主矩阵已判单核：5% 口径下 1 核即答案，不开会话。
                core_n = 1;
            } else if bsp != 0 {
                log::warn!(
                    "render auto: BSP who_am_i={} != 0, core stage fallback all {} cores",
                    bsp,
                    n_cores
                );
            } else {
                let champ_ticks = match winner_dist {
                    DistMode::Static => acc_static,
                    DistMode::Tile => acc_tile,
                    _ => acc_dynamic,
                };
                let mut ticks: [(usize, u64); 3] = [(0, 0); 3];
                let mut nt = 0usize;
                if time_direct && acc_direct > 0 {
                    ticks[nt] = (1, acc_direct);
                    nt += 1;
                }
                // half 格（与端点去重后若无独立格，即跳过会话；上限复用
                // `effective_crew_count`，与播放期 crew 口径一致）。
                let (cells, ncells) = config::core_cells(config::effective_crew_count(n_cores), 0);
                let half_ticks = cells[..ncells]
                    .iter()
                    .copied()
                    .find(|&c| c != 1 && c != n_cores)
                    .and_then(|k| {
                        self.time_crew_cell(
                            k,
                            winner_dist,
                            &pixels,
                            w,
                            h,
                            tps,
                            frames,
                            cal_sh,
                            &cal_round,
                            &cal_done,
                            &cal_mode,
                            &cal_tile,
                            cal_times.as_ptr(),
                            cal_bounds.as_ptr(),
                            mp_store.as_deref(),
                        )
                        .map(|ht| (k, ht))
                    });
                if let Some((k, ht)) = half_ticks {
                    ticks[nt] = (k, ht);
                    nt += 1;
                }
                if champ_ticks > 0 {
                    ticks[nt] = (n_cores, champ_ticks);
                    nt += 1;
                }
                core_n = config::pick_core_count(&ticks[..nt]);
                log::info!(
                    "render auto: core stage cells={:?} -> {} cores",
                    &ticks[..nt],
                    core_n
                );
            }
            if core_n <= 1 {
                // 5% 口径判单核更快：workers 翻回单核（`persist_*` 下行重算）。
                winner_workers = WorkersMode::BspOnly;
            }
        }

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

        let cell_str = |timed: bool, acc: u64| {
            if timed && acc > 0 {
                acc.to_string()
            } else {
                String::from("SKIP")
            }
        };
        log::info!(
            "render auto: direct_bsp {} ticks vs mp_static {} vs mp_tile {} vs mp_dynamic {} vs blt {} vs blt_op({:?}) {} ({} frames, {} cores, hybrid={}) -> render={:?} workers={:?} dist={:?} vsync={:?} (overruns {}/{})",
            if time_direct {
                acc_direct.to_string()
            } else {
                String::from("SKIP")
            },
            cell_str(time_static, acc_static),
            cell_str(time_tile, acc_tile),
            cell_str(time_dynamic, acc_dynamic),
            cell_str(time_blt, acc_blt),
            sel_op,
            cell_str(time_op, acc_op),
            frames,
            n_cores,
            !cal_sh.is_null(),
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
        // 核数记忆（`[schedule] measured_workers_n`；三态由上行同步写回做展示兼容）。
        if mem_eligible {
            match config::persist_measured_workers_n(core_n as u32) {
                Ok(()) => {}
                Err(e) => log::warn!("render auto: core persist failed: {:?}", e),
            }
        }
    }

    /// 核数 stage 单格计时：crew=k 新会话，在冠军 dist 路上跑 `frames` 轮。
    /// 复用主矩阵的像素表/影子槽/GOP 与 times/bounds 数组（主会话已 QUIT，
    /// 共享原子量进会话先清零）；任何失败回 None（调用方按已有格裁决）。
    /// BSP-id 假设见 `config::crew`（调用方已验 0，此处按 0 跑）。
    #[allow(clippy::too_many_arguments)]
    fn time_crew_cell(
        &mut self,
        k: usize,
        dist: DistMode,
        pixels: &[BltPixel],
        w: usize,
        h: usize,
        tps: u64,
        frames: u64,
        cal_sh: *mut u8,
        cal_round: &AtomicUsize,
        cal_done: &AtomicUsize,
        cal_mode: &AtomicU8,
        cal_tile: &AtomicUsize,
        times_ptr: *const AtomicUsize,
        bounds_ptr: *const AtomicUsize,
        mp: Option<&MpServices>,
    ) -> Option<u64> {
        let mp = mp?;
        let k = k.max(1);
        if k <= 1 {
            return None;
        }
        // 主会话残留（round=QUIT/done 累计）清零；mode/tile 由 drive 逐轮发布。
        cal_round.store(0, Ordering::SeqCst);
        cal_done.store(0, Ordering::SeqCst);
        let fb_base = self.gop.frame_buffer().as_mut_ptr();
        let fb_stride = self.gop.current_mode_info().stride() * 4;
        let draw_base = if cal_sh.is_null() { fb_base } else { cal_sh };
        let ctx = MpCalCtx {
            mp,
            fb_base: draw_base,
            fb_stride,
            src: pixels.as_ptr() as *const u8,
            row_bytes: w * 4,
            height: h,
            n: k,
            round: cal_round,
            done: cal_done,
            mode: cal_mode,
            tile: cal_tile,
            tile_rows: config::tune_tile_rows() as usize,
            tiles_total: h.div_ceil(config::tune_tile_rows().max(1) as usize),
            times: times_ptr,
            bounds: bounds_ptr,
            // crew 会话：只有 id < k 参加，余者 idle 到 QUIT（见 `mp_copy`）。
            cap: k,
        };
        let arg_ptr = Box::leak(Box::new(ctx)) as *mut _ as *mut c_void;
        let ev = unsafe { create_event(EventType::empty(), Tpl::CALLBACK, None, None) }.ok()?;
        mp.startup_all_aps(false, ap_cal_copy, arg_ptr, Some(ev), None)
            .ok()?;
        let (by0, by1) = rows_for_cal(0, k, h);
        // GOP 裸指针只给 BSP 调 Blt 整推用（AP 永不碰 GOP）；会话期 Screen 活着，安全。
        let gop_ptr = &mut *self.gop as *mut GraphicsOutput;
        let mut drive = MpDrive {
            n: k,
            bsp_id: 0,
            fb_base: draw_base,
            fb_stride,
            src: pixels.as_ptr() as *const u8,
            row_bytes: w * 4,
            height: h,
            bsp_y0: by0,
            bsp_y1: by1,
            expected: k.saturating_sub(1),
            round: cal_round,
            done: cal_done,
            mode: cal_mode,
            tile: cal_tile,
            tile_rows: config::tune_tile_rows() as usize,
            tiles_total: h.div_ceil(config::tune_tile_rows().max(1) as usize),
            times: times_ptr,
            bounds: bounds_ptr,
            real_fb: fb_base,
            gop: gop_ptr,
            blt_push: !cal_sh.is_null(),
            push_warned: false,
            next: 0,
            tps,
        };
        // 热身 + 计时（与主矩阵同口径：static/tile 热身一轮；dynamic 先 probe 定权）。
        let run = |drive: &mut MpDrive, dist: DistMode| match dist {
            DistMode::Static => drive.run_round(),
            DistMode::Tile => drive.run_round_tiled(),
            _ => drive.run_round_dynamic(),
        };
        match dist {
            DistMode::Static => drive.warmup_static(),
            DistMode::Tile => drive.warmup_tiled(),
            _ => {
                if !drive.probe_dynamic() {
                    cal_round.store(CAL_QUIT, Ordering::SeqCst);
                    return None;
                }
                drive.warmup_dynamic();
            }
        }
        let mut acc = 0u64;
        for _ in 0..frames.max(1) {
            let t = tsc();
            let ok = run(&mut drive, dist);
            let dt = tsc().saturating_sub(t);
            if !ok {
                cal_round.store(CAL_QUIT, Ordering::SeqCst);
                stall(Duration::from_millis(50));
                return None;
            }
            acc += dt;
        }
        cal_round.store(CAL_QUIT, Ordering::SeqCst);
        stall(Duration::from_millis(50));
        if acc == 0 { None } else { Some(acc) }
    }
}
