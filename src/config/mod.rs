//! `\player.toml` —— 加速矩阵配置（指令集档位 × 渲染路径）。
//!
//! 位置：启动卷根目录（与本程序写 `HWBENCH*.TXT` 同层）。
//! 解析：`toml` 1.x（no_std）+ serde。缺失/非法 → 全默认 + 屏显一行，不 halt。
//! P2 配置拆分：播放器侧（accel/render/schedule/play/feed/decode/display/
//! tune/media_dir）读 `runtime::PlayerRuntime`（开机 `seed` 后脱钩，TUI 经
//! `runtime_mut` 改内存）；壁纸侧（wallpaper_*/playlist_*）继续直读文件原子
//! （见 `wallpaper_io`）；boot_*/measured_* 不动（含落盘）。
//!
//! ```toml
//! version = 1
//! [accel]
//! level = "auto"  # 性能序：auto | scalar | sse..sse42 | popcnt/lzcnt/movbe/aes/
//!                 # pclmulqdq/rdrand/rdseed/sha/gfni | tbm/sse4a（AMD 专有垫底）
//!                 # | avx | f16c | fma | avx2 | bmi1 | bmi2 | avxvnni
//!                 # | avx512f | avx512cd | avx512dq | avx512bw | avx512vl
//!                 # auto=开机探最高可用；显式超硬件警告+钳住；旧 "avx512"/"on"/
//!                 # 整数壁纸 fps 皆整文件回默认 + 警告（无别名）。
//!                 # 解码 SIMD 无独立开关：六家单比特跟随全局（scalar 关/其余开）
//! [render]
//! mode    = "auto"        # "auto" | "direct" | "blt"
//! vsync   = "auto"        # "auto" | "static" | "half" | "off"（软件帧预算）
//! blt_op  = "buffer_to_video"
//! # 首启校准记忆（固件回写，GUI 只读；"auto"/0=未测，下次必测）：
//! # measured_render/workers/dist/vsync + measured_vsync_fps + measured_cores
//! # + [schedule] measured_workers_n（核数）
//! [render.tune]
//! frames    = 30   # 矩阵帧数（>=1）
//! tile_rows = 64   # tile 高（8-512）
//! vsync_fps = 60   # vsync 预算（15-240）
//! buffers   = 2    # 后缓冲（0=直写；1-16=影子环+整推；缺省 2）
//! window    = 4    # 流式供片全局默认（1-32）
//! [play]
//! fps = "auto"            # "auto" | "max" | "1".."240"
//! feed = "clip"           # "clip"|"staged"|"stream"|"zero_copy"|"single"
//! media_dir = ""          # P2新增：片源目录（""=根目录，单层非递归，不存在由扫描侧报错）
//! measured_max_fps = 0    # 固件实测后回写，GUI 只读
//! [playlist]              # 壁纸画廊列表兼播放列表（P2收敛唯一入口）
//! loop = "loop_list"      # "loop_single" | "loop_list"（默认）| "once"
//! [[playlist.items]]      # 单键片源路径（qois/raw/mp4探针hevc>avc>vp9/.hevc/.ivf/.h263/.m2v/.ogv）
//! path = "1080p/video.mp4"
//! [decode.hevc]           # 每格式解码表（八格式全键，缺表缺键全默认）：
//! threads = "bsp_only"    # "bsp_only"（默认）| "all"（qois/raw兑现）
//! zero_copy/streaming = false # 单选 hint
//! buffers = 0             # 0=跟随tune.window，1-32覆盖
//! fetch = "clip"          # 可选覆盖（缺键继承全局feed；壁纸恒clip忽略）
//! [wallpaper]             # P2破坏式：只留四键（enable/fps/preload/popup_freeze）；
//! enable = false          # 总开关（默认关；开但空=静默无壁纸）
//! fps = "15"              # 与[play]fps同构三态（默认"15"）
//! preload = 8             # 按项预载封顶（0=不封顶）
//! popup_freeze = true     # 弹窗冻结壁纸
//! # [[wallpaper.items]]/[wallpaper]loop 已删（残留忽略；画廊收敛到[playlist]）
//! [boot]
//! auto/windows/linux/shell/uki/macos = true  # 多卷自动发现总开关+种类
//! strict = false          # BLS只认本卷+XBOOTLDR
//! [schedule] backend = "manual" | "nano" | "dynamic"
//! workers = "auto"（auto | bsp_only | all）
//! dist = "auto"（auto | static | tile | dynamic）
//! cores = 0（0=不限；N=至多N核；1..64）
//! roles = "auto"（auto | combined | split）
//! measured_workers_n = 0（0=未测）
//! [async] executor = "all"  # 诊断项a跑哪条线："all"|"nano"|"dynamic"
//! ```

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use uefi::cstr16;

use crate::error::NyaStatus;
use crate::fs::Fs;

mod crew;
mod decode;
mod display;
mod runtime;
mod types;
mod wallpaper_io;

pub use crew::persist_measured_workers_n;
pub use crew::{
    ap_index_in_crew, core_cells, effective_crew_count, effective_roles, emit_sched_line,
    measured_workers_n, mp_slots_wanted, pick_core_count, resolve_crew_count, schedule_cores,
    schedule_roles, slots_eligible, slots_feed_eligible,
};
pub use decode::{
    decode_buffers, decode_effective_feed, decode_fetch, decode_raw_wh, decode_streaming,
    decode_threads, decode_window, decode_zero_copy, effective_feed,
};
pub use display::{BarColor, DisplayAlign, DisplayConfig, DisplayFit};
pub use runtime::{
    PlayerRuntime, accel_level, blt_op, copy_width, display, display_align, display_bar,
    display_fit, dist_mode, effective_dist, effective_render, effective_vsync, effective_vsync_fps,
    effective_workers, media_dir, play_feed, play_policy, render_mode, resolve_accel, runtime,
    runtime_mut, schedule_backend, seed_runtime_from_file, set_resolved_dist, set_resolved_render,
    set_resolved_vsync, set_resolved_vsync_fps, set_resolved_workers, tune_buffers, tune_frames,
    tune_tile_rows, tune_vsync_fps, tune_window, vsync_enabled, vsync_mode, workers_mode,
};
pub use types::*;
pub use wallpaper_io::{
    effective_wallpaper_fps, playlist_len, playlist_loop, playlist_path, save_wallpaper,
    set_wallpaper_enable, set_wallpaper_fps, set_wallpaper_popup_freeze, set_wallpaper_preload,
    wallpaper_enabled, wallpaper_fps_policy, wallpaper_frames, wallpaper_len, wallpaper_loop,
    wallpaper_path, wallpaper_popup_freeze, wallpaper_preload,
};

/// 解析结果：配置 + 可选的屏显警告（文件不存在不算错，返回 None）。
/// 非法值整文件回默认 + 警告（无别名/回退；未知键 serde 默认忽略）。
pub fn parse() -> (Config, Option<String>) {
    let mut fs = match Fs::new() {
        Ok(f) => f,
        Err(e) => {
            return (
                Config::default(),
                Some(format!("cfg: open volume failed: {:?}", e)),
            );
        }
    };

    let bytes = match fs.read_file(cstr16!("player.toml")) {
        Ok(b) => b,
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            return (Config::default(), None);
        }
        Err(e) => {
            return (
                Config::default(),
                Some(format!("cfg: read failed: {:?}", e)),
            );
        }
    };

    let text = String::from_utf8_lossy(&bytes);
    match toml::from_str::<Config>(&text) {
        Ok(cfg) => (cfg, None),
        Err(e) => (
            Config::default(),
            Some(format!("cfg: parse error: {} -- using defaults", e)),
        ),
    }
}

// ---------- 文件侧原子（boot/measured/async/play-measured；播放器侧已迁 runtime，壁纸侧已迁 wallpaper_io） ----------

/// auto 校准记忆（`Auto`/`0`=未测）：随配置加载，校准后同步更新内存。
static MEASURED_RENDER: AtomicU8 = AtomicU8::new(0);
static MEASURED_WORKERS: AtomicU8 = AtomicU8::new(0);
static MEASURED_VSYNC: AtomicU8 = AtomicU8::new(0);
static MEASURED_DIST: AtomicU8 = AtomicU8::new(0);
static MEASURED_CORES: AtomicU32 = AtomicU32::new(0);
/// 盘上 `measured_vsync_fps`（0=未测）。
static MEASURED_VSYNC_FPS: AtomicU32 = AtomicU32::new(0);
/// `[boot]` 自动发现开关（6 个 u8，0/1）。
static BOOT_AUTO: AtomicU8 = AtomicU8::new(1);
static BOOT_WINDOWS: AtomicU8 = AtomicU8::new(1);
static BOOT_LINUX: AtomicU8 = AtomicU8::new(1);
static BOOT_SHELL: AtomicU8 = AtomicU8::new(1);
static BOOT_UKI: AtomicU8 = AtomicU8::new(1);
static BOOT_MACOS: AtomicU8 = AtomicU8::new(1);
static BOOT_STRICT: AtomicU8 = AtomicU8::new(0);
/// `[async]` 执行器线（0=all，1=nano，2=dynamic；只驱动诊断项 `a`）。
static ASYNC_EXECUTOR: AtomicU8 = AtomicU8::new(0);
/// 固件实测的最大播放帧率（0=未测）。`auto` 首启测完写入并回盘。
static PLAY_MEASURED: AtomicU32 = AtomicU32::new(0);

/// 开机读一次；播放器侧经 `runtime::apply_cfg` 进 runtime（之后脱钩），
/// 壁纸/playlist 经 `wallpaper_io` 进文件原子，boot/measured 留此。
pub fn load() -> Option<String> {
    let (cfg, warn) = parse();
    // 播放器侧快照（accel/render/schedule/play/feed/decode/display/tune/media_dir）。
    runtime::apply_cfg(&cfg);
    // 壁纸侧直读文件（画廊收敛到 [playlist]；wallpaper 只留四键）。
    wallpaper_io::store_playlist(cfg.playlist.loop_mode, cfg.playlist.items.clone());
    wallpaper_io::store_wallpaper(
        cfg.wallpaper.enable,
        &cfg.wallpaper.fps,
        cfg.wallpaper.preload,
        cfg.wallpaper.popup_freeze,
    );
    // 调度核数记忆（`[schedule] measured_workers_n`）。
    crew::store_measured(cfg.schedule.measured_workers_n);
    MEASURED_RENDER.store(cfg.render.measured_render.as_u8(), Ordering::Relaxed);
    MEASURED_WORKERS.store(cfg.render.measured_workers.as_u8(), Ordering::Relaxed);
    MEASURED_DIST.store(cfg.render.measured_dist.as_u8(), Ordering::Relaxed);
    MEASURED_VSYNC.store(cfg.render.measured_vsync.as_u8(), Ordering::Relaxed);
    MEASURED_VSYNC_FPS.store(cfg.render.measured_vsync_fps, Ordering::Relaxed);
    MEASURED_CORES.store(cfg.render.measured_cores, Ordering::Relaxed);
    BOOT_AUTO.store(cfg.boot.auto as u8, Ordering::Relaxed);
    BOOT_WINDOWS.store(cfg.boot.windows as u8, Ordering::Relaxed);
    BOOT_LINUX.store(cfg.boot.linux as u8, Ordering::Relaxed);
    BOOT_SHELL.store(cfg.boot.shell as u8, Ordering::Relaxed);
    BOOT_UKI.store(cfg.boot.uki as u8, Ordering::Relaxed);
    BOOT_MACOS.store(cfg.boot.macos as u8, Ordering::Relaxed);
    BOOT_STRICT.store(cfg.boot.strict as u8, Ordering::Relaxed);
    ASYNC_EXECUTOR.store(cfg.r#async.executor.as_u8(), Ordering::Relaxed);
    PLAY_MEASURED.store(cfg.play.measured_max_fps, Ordering::Relaxed);
    log::info!(
        "cfg: version={} accel.level={:?} render.mode={:?} sched.backend={:?} workers={:?} dist={:?} cores={} roles={:?} wmem={} vsync={:?} blt_op={:?} tune.frames={} measured(render={:?} workers={:?} dist={:?} vsync={:?} vhalf={} cores={}) play.fps={} play.feed={:?} media_dir={} measured_max_fps={} playlist.items={} wallpaper.items={} async.executor={:?} boot(auto={} win={} lin={} sh={} uki={} mac={} strict={})",
        cfg.version,
        cfg.accel.level,
        cfg.render.mode,
        cfg.schedule.backend,
        cfg.schedule.workers,
        cfg.schedule.dist,
        cfg.schedule.cores,
        cfg.schedule.roles,
        cfg.schedule.measured_workers_n,
        cfg.render.vsync,
        cfg.render.blt_op,
        cfg.render.tune.frames,
        cfg.render.measured_render,
        cfg.render.measured_workers,
        cfg.render.measured_dist,
        cfg.render.measured_vsync,
        cfg.render.measured_vsync_fps,
        cfg.render.measured_cores,
        cfg.play.fps,
        cfg.play.feed,
        cfg.play.media_dir,
        cfg.play.measured_max_fps,
        playlist_len(),
        wallpaper_len(),
        cfg.r#async.executor,
        cfg.boot.auto,
        cfg.boot.windows,
        cfg.boot.linux,
        cfg.boot.shell,
        cfg.boot.uki,
        cfg.boot.macos,
        cfg.boot.strict,
    );
    log::info!(
        "playlist: items={} loop={}",
        playlist_len(),
        playlist_loop().as_str()
    );
    warn
}

/// 已记忆的 auto 矩阵结论（`Auto`/`0`=未测）。
pub fn measured_render() -> RenderMode {
    RenderMode::from_u8(MEASURED_RENDER.load(Ordering::Relaxed))
}
pub fn measured_workers() -> WorkersMode {
    WorkersMode::from_u8(MEASURED_WORKERS.load(Ordering::Relaxed))
}
pub fn measured_dist() -> DistMode {
    DistMode::from_u8(MEASURED_DIST.load(Ordering::Relaxed))
}
pub fn measured_vsync() -> VsyncMode {
    VsyncMode::from_u8(MEASURED_VSYNC.load(Ordering::Relaxed))
}
/// half 实测预算（fps；0=未测）。
pub fn measured_vsync_fps() -> u32 {
    MEASURED_VSYNC_FPS.load(Ordering::Relaxed)
}
/// 校准时机型 AP+BSP 总数（0=未测）。
pub fn measured_cores() -> u32 {
    MEASURED_CORES.load(Ordering::Relaxed)
}

/// `[boot]` 自动发现总开关（关=只列本卷 BLS）。
pub fn boot_auto() -> bool {
    BOOT_AUTO.load(Ordering::Relaxed) != 0
}
/// 自动发现 Windows Boot Manager。
pub fn boot_windows() -> bool {
    BOOT_WINDOWS.load(Ordering::Relaxed) != 0
}
/// 自动发现 Linux 内核对。
pub fn boot_linux() -> bool {
    BOOT_LINUX.load(Ordering::Relaxed) != 0
}
/// 自动发现 UEFI Shell。
pub fn boot_shell() -> bool {
    BOOT_SHELL.load(Ordering::Relaxed) != 0
}
/// 自动发现 UKI（`EFI/Linux/*.efi`）。
pub fn boot_uki() -> bool {
    BOOT_UKI.load(Ordering::Relaxed) != 0
}
/// OpenCore 链式接力（`\EFI\OC\OpenCore.efi` 存在即链）。
pub fn boot_macos() -> bool {
    BOOT_MACOS.load(Ordering::Relaxed) != 0
}
/// 严格模式：BLS 只认本卷 + XBOOTLDR 卷。
pub fn boot_strict() -> bool {
    BOOT_STRICT.load(Ordering::Relaxed) != 0
}

/// 诊断项 `a` 跑哪条执行器线（默认 all；只管诊断覆盖）。
pub fn async_executor() -> AsyncExecutor {
    AsyncExecutor::from_u8(ASYNC_EXECUTOR.load(Ordering::Relaxed))
}

/// 是否有 auto 维度尚未校准（任一 auto 无记忆即 true，首启必 true）。
pub fn needs_render_bench() -> bool {
    if render_mode() == RenderMode::Auto && measured_render() == RenderMode::Auto {
        return true;
    }
    let render_is_blt = match render_mode() {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => measured_render() == RenderMode::Blt,
    };
    if !render_is_blt
        && workers_mode() == WorkersMode::Auto
        && schedule_cores() == 0
        && measured_workers_n() == 0
    {
        return true;
    }
    let dist_in_play = !render_is_blt
        && workers_mode() != WorkersMode::BspOnly
        && !(workers_mode() == WorkersMode::Auto && measured_workers() == WorkersMode::BspOnly);
    if dist_in_play && dist_mode() == DistMode::Auto && measured_dist() == DistMode::Auto {
        return true;
    }
    if vsync_mode() == VsyncMode::Half && measured_vsync_fps() == 0 {
        return true;
    }
    let blt_in_play = match render_mode() {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => {
            measured_render() == RenderMode::Auto || measured_render() == RenderMode::Blt
        }
    };
    if blt_in_play && blt_op() != BltOpKind::BufferToVideo {
        return true;
    }
    vsync_mode() == VsyncMode::Auto && measured_vsync() == VsyncMode::Auto
}

/// 复用盘上记忆：把 `measured_*` 装进本启动的结论（跳过矩阵的快启路径）。
pub fn apply_measured_render() {
    runtime::apply_measured_inner(
        measured_render(),
        measured_workers(),
        measured_dist(),
        measured_vsync(),
        measured_vsync_fps(),
    );
}

/// 矩阵校准完把结论回写 `\player.toml`（`None` 的维度不动盘上记忆）。
pub fn persist_render_measured(
    render: Option<RenderMode>,
    workers: Option<WorkersMode>,
    dist: Option<DistMode>,
    vsync: Option<VsyncMode>,
    cores: u32,
) -> crate::error::Result {
    if let Some(m) = render {
        MEASURED_RENDER.store(m.as_u8(), Ordering::Relaxed);
        set_resolved_render(m);
    }
    if let Some(w) = workers {
        MEASURED_WORKERS.store(w.as_u8(), Ordering::Relaxed);
        set_resolved_workers(w);
    }
    if let Some(d) = dist {
        MEASURED_DIST.store(d.as_u8(), Ordering::Relaxed);
        set_resolved_dist(d);
    }
    if let Some(v) = vsync {
        MEASURED_VSYNC.store(v.as_u8(), Ordering::Relaxed);
        set_resolved_vsync(v);
    }
    if cores > 0 {
        MEASURED_CORES.store(cores, Ordering::Relaxed);
    }
    let mut fs = Fs::new()?;
    let mut cfg = match fs.read_file(cstr16!("player.toml")) {
        Ok(bytes) => toml::from_str::<Config>(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        Err(_) => Config::default(),
    };
    if let Some(m) = render {
        cfg.render.measured_render = m;
    }
    if let Some(w) = workers {
        cfg.render.measured_workers = w;
    }
    if let Some(d) = dist {
        cfg.render.measured_dist = d;
    }
    if let Some(v) = vsync {
        cfg.render.measured_vsync = v;
    }
    if cores > 0 {
        cfg.render.measured_cores = cores;
    }
    let text = toml::to_string(&cfg)
        .map_err(|_| NyaStatus::_Debug(String::from("cfg: serialize failed")))?;
    fs.write_file(cstr16!("player.toml"), text.as_bytes())?;
    Ok(())
}

/// 已实测的最大播放帧率（0=未测）。
pub fn measured_max_fps() -> u32 {
    PLAY_MEASURED.load(Ordering::Relaxed)
}

/// 供片路由垫片（只读 `mp_slots` 分发用）：首项路径扩展名初判，
/// 与 `play::detect_clip_format` 同序但免 I/O。空列表回默认 qois。
pub fn play_format() -> MediaFormat {
    let p = playlist_path(0).unwrap_or_default();
    let l = p.to_ascii_lowercase();
    if l.ends_with(".qois") {
        MediaFormat::Qois
    } else if is_raw_frame_path(&l) {
        MediaFormat::Raw
    } else if l.ends_with(".hevc") || l.ends_with(".265") || l.ends_with(".mp4") {
        MediaFormat::Hevc
    } else if l.ends_with(".ivf") {
        MediaFormat::Vp9
    } else if l.ends_with(".h263") {
        MediaFormat::H263
    } else if l.ends_with(".m2v") {
        MediaFormat::Mpeg2v
    } else if l.ends_with(".ogv") {
        MediaFormat::Theora
    } else {
        MediaFormat::Qois
    }
}

/// 实测完把最大帧率写回 `\player.toml`（GUI 下次打开即见）。
pub fn persist_measured_max_fps(m: u32) -> crate::error::Result {
    PLAY_MEASURED.store(m, Ordering::Relaxed);
    let mut fs = Fs::new()?;
    let mut cfg = match fs.read_file(cstr16!("player.toml")) {
        Ok(bytes) => toml::from_str::<Config>(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        Err(_) => Config::default(),
    };
    cfg.play.measured_max_fps = m;
    let text = toml::to_string(&cfg)
        .map_err(|_| NyaStatus::_Debug(String::from("cfg: serialize failed")))?;
    fs.write_file(cstr16!("player.toml"), text.as_bytes())?;
    Ok(())
}

/// half 定档完把预算写回 `\player.toml`（同时装进本启动的运行结论）。
pub fn persist_measured_vsync_fps(m: u32) -> crate::error::Result {
    MEASURED_VSYNC_FPS.store(m, Ordering::Relaxed);
    set_resolved_vsync_fps(m);
    let mut fs = Fs::new()?;
    let mut cfg = match fs.read_file(cstr16!("player.toml")) {
        Ok(bytes) => toml::from_str::<Config>(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        Err(_) => Config::default(),
    };
    cfg.render.measured_vsync_fps = m;
    let text = toml::to_string(&cfg)
        .map_err(|_| NyaStatus::_Debug(String::from("cfg: serialize failed")))?;
    fs.write_file(cstr16!("player.toml"), text.as_bytes())?;
    Ok(())
}
