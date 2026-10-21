//! `\player.toml` —— 加速矩阵配置（指令集档位 × 渲染路径）。
//!
//! 位置：启动卷根目录（与本程序写 `HWBENCH*.TXT` 同层）。
//! 解析：`toml` 1.x（no_std）+ serde。缺失/非法 → 全默认 + 屏显一行，不 halt。
//!
//! ```toml
//! version = 1
//! [accel]
//! level = "auto"  # 性能序（滑杆档位）：auto | scalar | sse | sse2 | sse3 | ssse3
//!                 # | sse41 | sse42 | popcnt | lzcnt | movbe | aes | pclmulqdq
//!                 # | rdrand | rdseed | sha | gfni | tbm | sse4a（AMD 专有垫底，
//!                 #   扩展叶 8000_0001H ECX bit 21/6；qemu64 无此叶，显式配了即
//!                 #   按 X128 钳住用，不断 boot）| avx | f16c | fma | avx2
//!                 # | bmi1 | bmi2 | avxvnni | avx512f | avx512cd | avx512dq
//!                 # | avx512bw | avx512vl
//!                 # auto = 开机探最高可用；显式超硬件警告+钳住；老 "avx512" 按 avx512f 照读
//!                 # 解码 SIMD 无独立开关：六家（hevc/vp9/h263/h264/mpeg2v/theora）单比特跟随全局
//!                 #（level=scalar 则关，其余开），见 `resolve_accel` 末尾
//! [render]
//! mode    = "auto"        # "auto" | "direct" | "blt"
//! workers = "auto"        # "auto" | "bsp_only" | "all"（多核开关）
//! dist    = "auto"        # "auto" | "static" | "tile" | "dynamic"（多核分工，只对 direct+all 有效）
//! vsync   = "auto"        # "auto" | "static" | "half" | "off"（软件帧预算，无硬件 vsync）
//! blt_op  = "buffer_to_video"
//! # 首启校准记忆（固件回写，GUI 只读展示；"auto"/0 = 未测，下次启动必测）：
//! measured_render  = "auto"  # "direct" | "blt"
//! measured_workers = "auto"  # "bsp_only" | "all"
//! measured_dist    = "auto"  # "static" | "tile" | "dynamic"
//! measured_vsync   = "auto"  # "static" | "off"（auto 探测结论；static/half 不用它）
//! measured_vsync_fps = 0    # half 实测预算（fps；0 = 未测，下次启动现测并回写）
//! measured_cores   = 0       # 校准时机型 AP+BSP 总数
//! [render.tune]
//! frames    = 30   # 每次矩阵/微基准的帧数（>=1）
//! tile_rows = 64   # tile 高（行），MP 自调度切块粒度（8-512）
//! vsync_fps = 60   # vsync 预算帧率：static 的封顶 + auto 探测达标线（15-240）
//! buffers   = 0    # 后缓冲帧数（0 = 直写前台，现状；1-16 = 影子环 + 整推，撕裂窗口压成一次 push）
//! [play]
//! fps = "auto"            # "auto" | "max" | "1".."240"
//! format = "qois"         # 片源格式："qois"（1080p/video.qois）| "raw"（1080p/frameNNNN.raw）
//!                         # | "hevc"（1080p/video.hevc，Annex-B 软解）| "vp9"（1080p/video.ivf 软解）
//!                         # | "h263" | "mpeg2v" | "theora"（格式可选、解码后继，选中即明确报错）
//! pipeline = "clip"       # 播放管线："clip"（预解码现路）| "staged" | "zero_copy" | "stream"
//!                         # | "single" | "legacy_blt" | "legacy_direct" | "legacy_u64"（老实现手动切）
//! measured_max_fps = 0    # 固件实测后回写，GUI 只读展示
//! [wallpaper]
//! enable = false          # 菜单背景 + 引导过渡总开关（默认关）
//! format = "raw"          # 片源格式："raw"（wallpaper/frameNNNN.raw）| "qois"（wallpaper/wall.qois）
//! fps = "15"               # 背景帧率三态（与 [play] fps 同构）："auto"（最左，
//!                         # 跟 vsync；vsync 关则不限速）| "max"（最右，不限速）
//!                         # | "1".."240"（中间显式封顶）；默认 "15"，老文件整数照读
//! frames = 0              # B 过渡封顶帧数（0 = 全播预载集）
//! preload = 8             # 预载帧数上限（1-32；0 = 不封顶，和播片一样拉满；
//!                         # 1080p BGRA 单帧约 8MB，长动画注意内存）
//! popup_freeze = true     # 选项弹窗时冻结壁纸（false 则行区继续播）
//! [boot]
//! auto = true             # 多卷自动发现总开关（关 = 只列本卷 BLS，老行为）
//! windows = true          # 自动发现 Windows Boot Manager
//! linux = true            # 自动发现 Linux 内核对（kernel + initrd)
//! shell = true            # 自动发现 UEFI Shell（shellx64.efi）
//! uki = true              # 自动发现 EFI/Linux/*.efi（读全套 profile，一 profile 一条目，读不到用文件名）
//! macos = true            # OpenCore 链式接力（EFI/OC/OpenCore.efi 存在即链）
//! strict = false          # 严格模式：BLS 只认本卷 + XBOOTLDR 卷（systemd 严格模式)
//! [schedule]
//! backend = "manual"      # 调度后端："manual"（手写 MP，workers/dist 生效）
//!                         # | "nano"（单核执行器泵，workers/dist 忽略）
//!                         # | "dynamic"（多核 steal 泵，只直写前台；见 fallbacks）
//! workers = "auto"        # manual 后端多核开关（auto | bsp_only | all）
//! dist = "auto"           # manual 后端分工（auto | static | tile | dynamic）
//! [async]
//! executor = "all"        # 诊断项 a 跑哪条执行器线："all"（默认，都跑）| "nano" | "dynamic"
//! ```

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use uefi::cstr16;

use crate::error::NyaStatus;
use crate::fs::Fs;

mod types;

pub use types::*;

/// 解析结果：配置 + 可选的屏显警告（文件不存在不算错，返回 None）
/// + `[schedule]` 表是否存在（`workers`/`dist` 搬家后的新老位置裁决用）。
pub fn parse() -> (Config, Option<String>, bool) {
    let mut fs = match Fs::new() {
        Ok(f) => f,
        Err(e) => {
            return (
                Config::default(),
                Some(format!("cfg: open volume failed: {:?}", e)),
                false,
            );
        }
    };

    let bytes = match fs.read_file(cstr16!("player.toml")) {
        Ok(b) => b,
        // 没有配置文件是正常情况：静默用默认。
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            return (Config::default(), None, false);
        }
        Err(e) => {
            return (
                Config::default(),
                Some(format!("cfg: read failed: {:?}", e)),
                false,
            );
        }
    };

    let text = String::from_utf8_lossy(&bytes);
    let has_schedule = toml::from_str::<toml::Table>(&text)
        .map(|t| t.contains_key("schedule"))
        .unwrap_or(false);
    match toml::from_str::<Config>(&text) {
        Ok(cfg) => (cfg, None, has_schedule),
        Err(e) => (
            Config::default(),
            Some(format!("cfg: parse error: {} -- using defaults", e)),
            false,
        ),
    }
}

// ---------- 生效值（原子存储；AP/任意处可读，无需 OnceLock） ----------

static ACCEL: AtomicU8 = AtomicU8::new(0);
static RENDER: AtomicU8 = AtomicU8::new(0);
static WORKERS: AtomicU8 = AtomicU8::new(0);
static DIST: AtomicU8 = AtomicU8::new(0);
static VSYNC: AtomicU8 = AtomicU8::new(0);
static BLTOP: AtomicU8 = AtomicU8::new(0);
/// auto 校准记忆（`Auto`/`0` = 未测）：随配置加载，校准后同步更新内存。
static MEASURED_RENDER: AtomicU8 = AtomicU8::new(0);
static MEASURED_WORKERS: AtomicU8 = AtomicU8::new(0);
static MEASURED_VSYNC: AtomicU8 = AtomicU8::new(0);
static MEASURED_DIST: AtomicU8 = AtomicU8::new(0);
static MEASURED_CORES: AtomicU32 = AtomicU32::new(0);
/// 盘上 `measured_vsync_fps`（0 = 未测）。
static MEASURED_VSYNC_FPS: AtomicU32 = AtomicU32::new(0);
static WALLPAPER_ENABLE: AtomicU8 = AtomicU8::new(0);
/// 0 = auto，1 = capped，2 = max（`wallpaper.fps` 的解析结果，与 `PLAY_MODE` 同构）。
static WALLPAPER_MODE: AtomicU8 = AtomicU8::new(1);
static WALLPAPER_CAP: AtomicU32 = AtomicU32::new(15);
static WALLPAPER_FRAMES: AtomicU32 = AtomicU32::new(0);
static WALLPAPER_PRELOAD: AtomicU32 = AtomicU32::new(default_wallpaper_preload());
static WALLPAPER_POPUP_FREEZE: AtomicU8 = AtomicU8::new(1);
/// `[boot]` 自动发现开关（5 个 u8，0/1）。
static BOOT_AUTO: AtomicU8 = AtomicU8::new(1);
static BOOT_WINDOWS: AtomicU8 = AtomicU8::new(1);
static BOOT_LINUX: AtomicU8 = AtomicU8::new(1);
static BOOT_SHELL: AtomicU8 = AtomicU8::new(1);
static BOOT_UKI: AtomicU8 = AtomicU8::new(1);
static BOOT_MACOS: AtomicU8 = AtomicU8::new(1);
static BOOT_STRICT: AtomicU8 = AtomicU8::new(0);
/// `[schedule]` 调度后端（0 = manual，1 = nano，2 = dynamic）。
static SCHED_BACKEND: AtomicU8 = AtomicU8::new(0);
/// `[async]` 执行器线（0 = all，1 = nano，2 = dynamic；只驱动诊断项 `a`）。
static ASYNC_EXECUTOR: AtomicU8 = AtomicU8::new(0);
static TUNE_FRAMES: AtomicU32 = AtomicU32::new(default_frames());
static TUNE_TILE_ROWS: AtomicU32 = AtomicU32::new(default_tile_rows());
static TUNE_VSYNC_FPS: AtomicU32 = AtomicU32::new(default_vsync_fps());
/// 后缓冲帧数（0-16，0 = 直写）。显式旋钮，不进矩阵 bench。
static TUNE_BUFFERS: AtomicU32 = AtomicU32::new(default_buffers());
/// 0 = auto，1 = capped，2 = max（`play.fps` 的解析结果）。
static PLAY_MODE: AtomicU8 = AtomicU8::new(0);
static PLAY_CAP: AtomicU32 = AtomicU32::new(0);
/// 固件实测的最大播放帧率（0 = 未测）。`auto` 首启测完写入并回盘。
static PLAY_MEASURED: AtomicU32 = AtomicU32::new(0);
/// 片源格式（0 = qois，1 = raw；`[play]` 默认 qois，`[wallpaper]` 默认 raw）。
static PLAY_FORMAT: AtomicU8 = AtomicU8::new(0);
/// 播放管线（`PlayPipeline` 判别值；0 = clip 默认）。
static PLAY_PIPELINE: AtomicU8 = AtomicU8::new(0);
static WALLPAPER_FORMAT: AtomicU8 = AtomicU8::new(1);
/// 0 = 未测，1 = direct，2 = blt（`mode = "auto"` 的矩阵结论，校准/记忆二选一写入）
static RESOLVED_RENDER: AtomicU8 = AtomicU8::new(0);
/// 0 = 未测，1 = bsp_only，2 = all（`workers = "auto"` 的矩阵结论）
static RESOLVED_WORKERS: AtomicU8 = AtomicU8::new(0);
/// 0 = 未测，1 = static，2 = off（`vsync = "auto"` 的探测结论）
static RESOLVED_VSYNC: AtomicU8 = AtomicU8::new(0);
/// half 实测预算（fps；0 = 未测）：`vsync = "half"` 首启现测取半后写入 + 回盘。
static RESOLVED_VSYNC_FPS: AtomicU32 = AtomicU32::new(0);
/// 0 = 未测，1 = static，2 = tile，3 = dynamic（`dist = "auto"` 的矩阵结论）
static RESOLVED_DIST: AtomicU8 = AtomicU8::new(0);
/// 硬件实测最大搬运位宽（`CopyWidth` 判别值；0 = 未探，按 X128 保守用）。
/// `auto` 取它，显式档位超出它会被钳住并警告（不断 boot）。
static MAX_ACCEL: AtomicU8 = AtomicU8::new(0);

/// 开机读一次；返回需要屏显的警告行。
pub fn load() -> Option<String> {
    let (cfg, warn, has_schedule) = parse();
    ACCEL.store(cfg.accel.level.as_u8(), Ordering::Relaxed);
    RENDER.store(cfg.render.mode.as_u8(), Ordering::Relaxed);
    // workers/dist 搬家裁决：`[schedule]` 表存在用新位置，否则认 `[render]` 旧键
    // （缺表老文件；显式旧键给一行警告后照读，不写回）。
    let (workers, dist) = if has_schedule {
        (cfg.schedule.workers, cfg.schedule.dist)
    } else {
        (cfg.render.workers, cfg.render.dist)
    };
    WORKERS.store(workers.as_u8(), Ordering::Relaxed);
    DIST.store(dist.as_u8(), Ordering::Relaxed);
    SCHED_BACKEND.store(cfg.schedule.backend.as_u8(), Ordering::Relaxed);
    VSYNC.store(cfg.render.vsync.as_u8(), Ordering::Relaxed);
    MEASURED_RENDER.store(cfg.render.measured_render.as_u8(), Ordering::Relaxed);
    MEASURED_WORKERS.store(cfg.render.measured_workers.as_u8(), Ordering::Relaxed);
    MEASURED_DIST.store(cfg.render.measured_dist.as_u8(), Ordering::Relaxed);
    MEASURED_VSYNC.store(cfg.render.measured_vsync.as_u8(), Ordering::Relaxed);
    MEASURED_VSYNC_FPS.store(cfg.render.measured_vsync_fps, Ordering::Relaxed);
    MEASURED_CORES.store(cfg.render.measured_cores, Ordering::Relaxed);
    BLTOP.store(cfg.render.blt_op as u8, Ordering::Relaxed);
    TUNE_FRAMES.store(cfg.render.tune.frames.max(1), Ordering::Relaxed);
    TUNE_TILE_ROWS.store(cfg.render.tune.tile_rows.clamp(8, 512), Ordering::Relaxed);
    TUNE_VSYNC_FPS.store(cfg.render.tune.vsync_fps.clamp(15, 240), Ordering::Relaxed);
    TUNE_BUFFERS.store(cfg.render.tune.buffers.min(16), Ordering::Relaxed);
    match parse_fps(&cfg.play.fps) {
        FpsPolicy::Auto => PLAY_MODE.store(0, Ordering::Relaxed),
        FpsPolicy::Capped(n) => {
            PLAY_MODE.store(1, Ordering::Relaxed);
            PLAY_CAP.store(n, Ordering::Relaxed);
        }
        FpsPolicy::Max => PLAY_MODE.store(2, Ordering::Relaxed),
    }
    PLAY_MEASURED.store(cfg.play.measured_max_fps, Ordering::Relaxed);
    PLAY_FORMAT.store(cfg.play.format.as_u8(), Ordering::Relaxed);
    PLAY_PIPELINE.store(cfg.play.pipeline.as_u8(), Ordering::Relaxed);
    WALLPAPER_FORMAT.store(cfg.wallpaper.format.as_u8(), Ordering::Relaxed);
    WALLPAPER_ENABLE.store(cfg.wallpaper.enable as u8, Ordering::Relaxed);
    match parse_fps(&cfg.wallpaper.fps) {
        FpsPolicy::Auto => WALLPAPER_MODE.store(0, Ordering::Relaxed),
        FpsPolicy::Capped(n) => {
            WALLPAPER_MODE.store(1, Ordering::Relaxed);
            WALLPAPER_CAP.store(n, Ordering::Relaxed);
        }
        FpsPolicy::Max => WALLPAPER_MODE.store(2, Ordering::Relaxed),
    }
    WALLPAPER_FRAMES.store(cfg.wallpaper.frames, Ordering::Relaxed);
    WALLPAPER_PRELOAD.store(cfg.wallpaper.preload.min(32), Ordering::Relaxed);
    WALLPAPER_POPUP_FREEZE.store(cfg.wallpaper.popup_freeze as u8, Ordering::Relaxed);
    BOOT_AUTO.store(cfg.boot.auto as u8, Ordering::Relaxed);
    BOOT_WINDOWS.store(cfg.boot.windows as u8, Ordering::Relaxed);
    BOOT_LINUX.store(cfg.boot.linux as u8, Ordering::Relaxed);
    BOOT_SHELL.store(cfg.boot.shell as u8, Ordering::Relaxed);
    BOOT_UKI.store(cfg.boot.uki as u8, Ordering::Relaxed);
    BOOT_MACOS.store(cfg.boot.macos as u8, Ordering::Relaxed);
    BOOT_STRICT.store(cfg.boot.strict as u8, Ordering::Relaxed);
    ASYNC_EXECUTOR.store(cfg.r#async.executor.as_u8(), Ordering::Relaxed);
    log::info!(
        "cfg: version={} accel.level={:?} render.mode={:?} sched.backend={:?} workers={:?} dist={:?} vsync={:?} blt_op={:?} tune.frames={} measured(render={:?} workers={:?} dist={:?} vsync={:?} vhalf={} cores={}) play.fps={} play.fmt={:?} measured_max_fps={} wallpaper.fmt={:?} async.executor={:?} boot(auto={} win={} lin={} sh={} uki={} mac={} strict={})",
        cfg.version,
        cfg.accel.level,
        cfg.render.mode,
        cfg.schedule.backend,
        workers,
        dist,
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
        cfg.play.format,
        cfg.play.measured_max_fps,
        cfg.wallpaper.format,
        cfg.r#async.executor,
        cfg.boot.auto,
        cfg.boot.windows,
        cfg.boot.linux,
        cfg.boot.shell,
        cfg.boot.uki,
        cfg.boot.macos,
        cfg.boot.strict,
    );
    // 缺表老文件显式写了旧键：认旧值 + 提示搬家（不写回，下次 GUI 存盘即正）。
    if !has_schedule
        && (cfg.render.workers != WorkersMode::Auto || cfg.render.dist != DistMode::Auto)
    {
        return Some(String::from(
            "cfg: [render] workers/dist moved to [schedule], honored this boot",
        ));
    }
    warn
}

pub fn accel_level() -> AccelLevel {
    AccelLevel::from_u8(ACCEL.load(Ordering::Relaxed))
}

pub fn render_mode() -> RenderMode {
    RenderMode::from_u8(RENDER.load(Ordering::Relaxed))
}

/// 调度后端（默认 manual；nano/dynamic 走执行器泵，见 `video::exec`）。
pub fn schedule_backend() -> ScheduleBackend {
    ScheduleBackend::from_u8(SCHED_BACKEND.load(Ordering::Relaxed))
}

pub fn blt_op() -> BltOpKind {
    match BLTOP.load(Ordering::Relaxed) {
        1 => BltOpKind::VideoToVideo,
        2 => BltOpKind::VideoFill,
        3 => BltOpKind::VideoToBltBuffer,
        _ => BltOpKind::BufferToVideo,
    }
}

pub fn tune_frames() -> u32 {
    TUNE_FRAMES.load(Ordering::Relaxed)
}

/// tile 高（行），已钳制 8-512。
pub fn tune_tile_rows() -> u32 {
    TUNE_TILE_ROWS.load(Ordering::Relaxed).clamp(8, 512).max(1)
}

/// vsync 预算帧率（`static` 的封顶 + `auto` 探测的达标线），已钳制 15-240。
pub fn tune_vsync_fps() -> u32 {
    TUNE_VSYNC_FPS.load(Ordering::Relaxed).clamp(15, 240).max(1)
}

/// 后缓冲帧数（0-16，已钳制）：0 = 直写前台（现状）；`>0` = 影子环 + 整推。
pub fn tune_buffers() -> u32 {
    TUNE_BUFFERS.load(Ordering::Relaxed).min(16)
}

/// 开机微基准测完把结论写进来。
pub fn set_resolved_render(m: RenderMode) {
    RESOLVED_RENDER.store(m.as_u8(), Ordering::Relaxed);
}

/// 实际生效的渲染路径：`auto` 用矩阵结论（未测则退回 direct）。
pub fn effective_render() -> RenderMode {
    match render_mode() {
        RenderMode::Auto => match RESOLVED_RENDER.load(Ordering::Relaxed) {
            2 => RenderMode::Blt,
            _ => RenderMode::Direct,
        },
        explicit => explicit,
    }
}

pub fn workers_mode() -> WorkersMode {
    WorkersMode::from_u8(WORKERS.load(Ordering::Relaxed))
}

pub fn dist_mode() -> DistMode {
    DistMode::from_u8(DIST.load(Ordering::Relaxed))
}

pub fn vsync_mode() -> VsyncMode {
    VsyncMode::from_u8(VSYNC.load(Ordering::Relaxed))
}

/// 实际生效的多核开关：`auto` 用矩阵结论（未测退回全核；Blt 路径忽略本项）。
pub fn effective_workers() -> WorkersMode {
    match workers_mode() {
        WorkersMode::Auto => match RESOLVED_WORKERS.load(Ordering::Relaxed) {
            1 => WorkersMode::BspOnly,
            _ => WorkersMode::All,
        },
        explicit => explicit,
    }
}

/// 实际生效的分工：直写 + 全核才有意义，其余路径恒返回 `Tile`（无害默认，
/// 调用方只在 `mp_draw` 里读）。`auto` 用矩阵结论（未测退回 tile）。
pub fn effective_dist() -> DistMode {
    if effective_render() != RenderMode::Direct || effective_workers() != WorkersMode::All {
        return DistMode::Tile;
    }
    match dist_mode() {
        DistMode::Auto => match RESOLVED_DIST.load(Ordering::Relaxed) {
            1 => DistMode::Static,
            3 => DistMode::Dynamic,
            _ => DistMode::Tile,
        },
        explicit => explicit,
    }
}

/// 实际生效的垂直同步：`auto` 用探测结论（未测退回关，只按 `play.fps` 跑）；
/// `static`/`half` 即开（预算见 `effective_vsync_fps`）。
pub fn effective_vsync() -> VsyncMode {
    match vsync_mode() {
        VsyncMode::Auto => match RESOLVED_VSYNC.load(Ordering::Relaxed) {
            1 => VsyncMode::Static,
            _ => VsyncMode::Off,
        },
        explicit => explicit,
    }
}

/// 实际生效的垂直同步预算（fps）：`static` 取 `tune.vsync_fps` 滑杆；
/// `half` 取本机实测取半（`RESOLVED_VSYNC_FPS`，0 兜底回滑杆值）；
/// `auto` 探测开时封顶同滑杆。调用方（播放限速）只认这个数，不认 60 硬编码。
pub fn effective_vsync_fps() -> u32 {
    match vsync_mode() {
        VsyncMode::Half => RESOLVED_VSYNC_FPS.load(Ordering::Relaxed),
        _ => tune_vsync_fps(),
    }
    .max(1)
}

/// vsync 开关量（播放限速用）：`static`/`half` 常开，`auto` 看探测结论。
pub fn vsync_enabled() -> bool {
    match effective_vsync() {
        VsyncMode::Static | VsyncMode::Half => true,
        VsyncMode::Auto | VsyncMode::Off => false,
    }
}

/// 开机探一次硬件最大搬运位宽（P2）：先 `ensure_xsave_enabled`（CPL0 直接开，
/// 幂等），再按 CPUID/XCR0 取最高可用。调一次，`upload_frame` 每次读结论。
/// 必须在任何上屏/bench 之前调（`main` 里调，bench/播放只读）。
/// 末尾把 `[accel]` 开关推进六家解码轮子：单比特跟随全局搬运位宽
/// （`copy_width() == U64`（即 `level=scalar`）则关，其余开）；
/// 首解前各轮子自保（`hevc_probe`/`avc_probe` 的幂等 `ensure` + vp9 轮内镜像门）保持。
pub fn resolve_accel() {
    #[cfg(target_arch = "x86_64")]
    {
        crate::diag::common::ensure_xsave_enabled();
        let max = probe_max_width();
        MAX_ACCEL.store(max as u8, Ordering::Relaxed);
        // 显式档位超硬件：警告 + 钳住（不断 boot，兼容兜底）。
        let want = accel_level();
        if want != AccelLevel::Auto && level_width(want) as u8 > max as u8 {
            log::warn!(
                "accel: level {:?} beyond hardware ({:?}), clamped",
                want,
                max_width_name(max)
            );
        }
        log::info!("accel: hardware max width = {:?}", max_width_name(max));
        // 解码轮子的运行时检测就是这道固件门（轮内无第二套 CPUID/XCR0 探）：
        // 五家单比特跟随全局 `copy_width()`（`scalar`=关/其余=开），首解前生效；
        // h264 例外收敛到 Y256/Z512（见下）。
        // `V128`（AVX/F16C）在 hevc 轮侧无 VEX-128 档，取其下最高档 `Sse41`
        //（AVX 机器恒有 SSE4.1，可安全执行）。
        let on = copy_width() != CopyWidth::U64;
        push_hevc_isa(copy_width());
        log::info!("accel: hevc decode isa = {}", rusty_h265::accel::describe());
        // h264（vendor 的 `rusty_h264_accel`，`set_simd_enabled` 单比特门；
        // no_std 下 `have_avx2/have_sse41` 恒取最高档（真门就是这里的一推，
        // 轮内无第二套探，见该目录 VENDOR.md），故只在 AVX2+XCR0 已验证的
        // Y256/Z512 才开（`probe_max_width` 的 Y256 门 = AVX 门 + AVX2 硬件位，
        // 缺一个即钳住，不断 boot）；其余位宽驻标量保正确（QEMU qemu64 实锤
        // 过开开关即 VEX #UD，见 `docs/simd.md` AVC 节）。
        // `describe()` 供 QEMU 矩阵断言（hevc 同款 `isa` 行）。
        let h264_on = matches!(copy_width(), CopyWidth::Y256 | CopyWidth::Z512);
        rusty_h264_accel::set_simd_enabled(h264_on);
        log::info!("accel: h264 decode isa = {}", rusty_h264_accel::describe());
        // vp9（`vp9dec::simd`，x86_64 内核常编，运行时单比特门）与
        // h263（`yuv`+`deblock` 的 `set_simd_enabled`，`simd` 特性下 wide 核，
        // 关特性时调用即 no-op）：同推单比特语义（`scalar`=关/其余=开）。
        vp9dec::simd::set_simd_allowed(on);
        h263_rs_yuv::bt601::set_simd_enabled(on);
        h263_rs_deblock::deblock::set_simd_enabled(on);
        // mpeg2v（`filmcraft_mpeg2v::set_simd_enabled`，`simd` 特性下整数 SSE2 核，
        // 关特性时调用即 no-op）与 theora（`oxideav_theora::simd::set_simd_enabled`，
        // `simd` 特性下 wide 整数核，关特性时调用即 no-op）：
        // 同推单比特语义（`scalar`=关/其余=开，h263 双箱同款）。
        filmcraft_mpeg2v::set_simd_enabled(on);
        oxideav_theora::simd::set_simd_enabled(on);
        log::info!(
            "accel: decode simd hevc={} vp9={} h263={} h264={} mpeg2v={} theora={}",
            onoff(on),
            onoff(on),
            onoff(on),
            onoff(h264_on),
            onoff(on),
            onoff(on),
        );
    }
}

/// hevc 轮子按位宽推 ISA 上限（`U64`=驻标量，其余见既有映射）。
fn push_hevc_isa(w: CopyWidth) {
    match w {
        CopyWidth::U64 => rusty_h265::accel::force_scalar(),
        CopyWidth::X128 => rusty_h265::accel::force_isa(rusty_h265::accel::Isa::Baseline),
        CopyWidth::V128 => rusty_h265::accel::force_isa(rusty_h265::accel::Isa::Sse41),
        CopyWidth::Y256 | CopyWidth::Z512 => {
            rusty_h265::accel::force_isa(rusty_h265::accel::Isa::Avx2)
        }
    }
}

fn onoff(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// 实际生效的搬运位宽：`auto` 取硬件最大；显式档位钳到硬件最大。
pub fn copy_width() -> CopyWidth {
    let max = hw_max_width();
    let want = match accel_level() {
        AccelLevel::Auto => max,
        explicit => level_width(explicit),
    };
    if want as u8 > max as u8 { max } else { want }
}

/// 硬件探到的最大搬运位宽（`resolve_accel` 的结论；显式档位超它即警告+钳住）。
fn hw_max_width() -> CopyWidth {
    CopyWidth::from_u8(MAX_ACCEL.load(Ordering::Relaxed))
}

/// 档位→搬运位宽（`target_feature` 小写无点形命名；位号依据 Intel SDM Vol.2A
/// CPUID，见 `docs/simd.md` 档位→位宽表）：
/// - `sse`~`sse42` → X128（128 位同宽；`sse` 早于 `sse2` 但 x86-64 恒有 SSE2，
///   `copy_block_x128` 的 `movdqu` 可安全执行）；
/// - `avx`/`f16c` → V128（VEX-128；F16C 本就要求 AVX，缺 AVX 即被钳住）；
/// - `avx2`/`fma`/`bmi1`/`bmi2`/`avxvnni` → Y256（VEX-256 时代档；
///   BMI 本是标量位操作，按时代下限同宽处理，缺 AVX2 即被钳住）；
/// - `avx512f/cd/dq/bw/vl` → Z512（512 位；`vmovdqu8` 引擎，探针按 F+XCR0 门控）；
/// - 标量/密码/计数类（`popcnt`/`aes`/`pclmulqdq`/`lzcnt`/`movbe`/`rdrand`/
///   `rdseed`/`sha`/`gfni`）+ AMD 专有（`tbm`/`sse4a`，扩展叶 8000_0001H
///   ECX bit 21/6）不加宽搬运，按 x86-64 基线下限 X128 处理。
fn level_width(l: AccelLevel) -> CopyWidth {
    match l {
        AccelLevel::Auto => CopyWidth::X128,
        AccelLevel::Scalar => CopyWidth::U64,
        AccelLevel::Sse
        | AccelLevel::Sse2
        | AccelLevel::Sse3
        | AccelLevel::Ssse3
        | AccelLevel::Sse41
        | AccelLevel::Sse42
        | AccelLevel::Popcnt
        | AccelLevel::Aes
        | AccelLevel::Pclmulqdq
        | AccelLevel::Lzcnt
        | AccelLevel::Movbe
        | AccelLevel::Rdrand
        | AccelLevel::Rdseed
        | AccelLevel::Sha
        | AccelLevel::Gfni
        // AMD 专有（扩展叶 8000_0001H ECX bit 21 tbm / bit 6 sse4a）：
        // 128 位/标量类档，按基线下限 X128 处理（已有 `popcnt`/`aes` 先例）。
        | AccelLevel::Tbm
        | AccelLevel::Sse4a => CopyWidth::X128,
        AccelLevel::Avx | AccelLevel::F16c => CopyWidth::V128,
        AccelLevel::Fma
        | AccelLevel::Avx2
        | AccelLevel::Bmi1
        | AccelLevel::Bmi2
        | AccelLevel::Avxvnni => CopyWidth::Y256,
        AccelLevel::Avx512f
        | AccelLevel::Avx512cd
        | AccelLevel::Avx512dq
        | AccelLevel::Avx512bw
        | AccelLevel::Avx512vl => CopyWidth::Z512,
    }
}

fn max_width_name(w: CopyWidth) -> &'static str {
    match w {
        CopyWidth::U64 => "scalar-u64",
        CopyWidth::X128 => "sse2-128",
        CopyWidth::V128 => "avx-128",
        CopyWidth::Y256 => "avx2-256",
        CopyWidth::Z512 => "avx512-512",
    }
}

/// CPUID/XCR0 探最高可用位宽（P2 矩阵的“探”；bench 只出数，选择看这里）。
/// 判定位与 `hw_bench`/`gop_simd` 同源（AVX512 五位门、`check_avx_support`）。
/// 读叶前先用最大叶号门控（与 `hw_bench::cpuid_simd` 同式，老 CPU 直接读
/// 7 号叶/扩展叶会回落误报）：缺叶恒取低位宽，不断 boot。
/// 各档位宽见 `level_width`；标量/密码类档不加宽（恒 ≤X128），探针只定上限。
#[cfg(target_arch = "x86_64")]
fn probe_max_width() -> CopyWidth {
    use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

    let c1 = __cpuid(1);
    let has7 = crate::diag::common::cpuid_max_basic() >= 7;
    let b7 = if has7 { __cpuid_count(7, 0).ebx } else { 0 };
    // 7 号叶 ECX（avxvnni/gfni）与扩展叶 0x80000001（ECX.5 lzcnt、ECX.6 sse4a、
    // ECX.21 tbm）不影响位宽上限（avxvnni/gfni 恒 ≤Y256 且必有同代 AVX2；
    // lzcnt/sse4a/tbm 是 X128 下限档），故不参与上限判定；
    // 真要读它们时必须先用 `cpuid_max_basic/_extended` 门控（`hw_bench` 同式）。
    // 扩展叶存在性门控读一遍（qemu64 无此叶即 0，不断 boot）：
    // tbm（ECX.21）/sse4a（ECX.6）显式配置恒按 X128 钳住用（见 `level_width`），
    // 矩阵在 qemu64 上实测即此路径（缺叶→钳住→不断）。
    let ext_ecx = if crate::diag::common::cpuid_max_extended() >= 0x8000_0001 {
        __cpuid(0x8000_0001).ecx
    } else {
        0
    };
    log::info!("accel: ext 80000001h ecx={:#010x}", ext_ecx);
    // AVX512：F 硬件位 + OSXSAVE + XCR0 五位全开，缺一个执行就 #UD。
    // 注：Z512 引擎是 `vmovdqu8`（BW 编码），探针按 F 门控（与 `gop_simd` 同口径；
    // 仅 F 无 BW 的硬件（KNL）不在本仓目标内，见 `docs/simd.md`）。
    if has7 && b7 & (1 << 16) != 0 && c1.ecx & (1 << 27) != 0 {
        let xcr0 = unsafe { _xgetbv(0) };
        const ZMM_MASK: u64 = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 7);
        if xcr0 & ZMM_MASK == ZMM_MASK {
            return CopyWidth::Z512;
        }
    }
    // Y256：AVX 门（`avx_checker`：AVX 硬件位 + OSXSAVE + XCR0.2）+ AVX2 硬件位。
    if crate::diag::avx_checker::check_avx_support() && has7 && b7 & (1 << 5) != 0 {
        return CopyWidth::Y256;
    }
    // V128：AVX 门（F16C 本就要求 AVX，同门覆盖）。
    if crate::diag::avx_checker::check_avx_support() {
        return CopyWidth::V128;
    }
    // X128：SSE2（x86-64 psABI 基线；sse/密码类档的下限）。
    if c1.edx & (1 << 26) != 0 {
        return CopyWidth::X128;
    }
    CopyWidth::U64
}

/// 已记忆的 auto 矩阵结论（`Auto`/`0` = 未测）。
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
/// half 实测预算（fps；0 = 未测）。
pub fn measured_vsync_fps() -> u32 {
    MEASURED_VSYNC_FPS.load(Ordering::Relaxed)
}
/// 校准时机型 AP+BSP 总数（0 = 未测）。
pub fn measured_cores() -> u32 {
    MEASURED_CORES.load(Ordering::Relaxed)
}

/// 壁纸总开关（P4-A 菜单背景 + P4-B 引导过渡共用）。
pub fn wallpaper_enabled() -> bool {
    WALLPAPER_ENABLE.load(Ordering::Relaxed) != 0
}
/// 壁纸片源格式（默认 raw；qois 读 `wallpaper/wall.qois`）。
pub fn wallpaper_format() -> MediaFormat {
    MediaFormat::from_u8(WALLPAPER_FORMAT.load(Ordering::Relaxed))
}
/// 选项弹窗时冻结壁纸（默认 true；false 则行区继续播）。
pub fn wallpaper_popup_freeze() -> bool {
    WALLPAPER_POPUP_FREEZE.load(Ordering::Relaxed) != 0
}
/// 开机解析出的壁纸帧率开关（与 `play_policy` 同构）。
pub fn wallpaper_fps_policy() -> FpsPolicy {
    match WALLPAPER_MODE.load(Ordering::Relaxed) {
        1 => FpsPolicy::Capped(WALLPAPER_CAP.load(Ordering::Relaxed).max(1)),
        2 => FpsPolicy::Max,
        _ => FpsPolicy::Auto,
    }
}

/// 实际生效的壁纸帧预算（`None` = 不限速）：`capped` 直接用（1-240 钳制）；
/// `max` 不限；`auto` 跟 vsync（开则按 `effective_vsync_fps()`，关则不限）。
/// 调用方（待机/弹窗/菜单背景/引导过渡）只认这个数，预算 0 即不限速。
pub fn effective_wallpaper_fps() -> Option<u32> {
    match wallpaper_fps_policy() {
        FpsPolicy::Max => None,
        FpsPolicy::Capped(n) => Some(n.clamp(1, 240).max(1)),
        FpsPolicy::Auto => {
            if vsync_enabled() {
                Some(effective_vsync_fps())
            } else {
                None
            }
        }
    }
}
/// B 过渡封顶帧数（0 = 全播预载集）。
pub fn wallpaper_frames() -> u32 {
    WALLPAPER_FRAMES.load(Ordering::Relaxed)
}
/// 预载帧数上限（0 = 不封顶；1-32，已钳制；单帧约 8MB，内存兜底）。
pub fn wallpaper_preload() -> u32 {
    WALLPAPER_PRELOAD.load(Ordering::Relaxed).min(32)
}

/// `[boot]` 自动发现总开关（关 = 只列本卷 BLS，老行为）。
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
/// 严格模式：BLS 只认本卷 + XBOOTLDR 卷（systemd 严格模式）。
pub fn boot_strict() -> bool {
    BOOT_STRICT.load(Ordering::Relaxed) != 0
}

/// 诊断项 `a` 跑哪条执行器线（默认 all；只管诊断覆盖）。
pub fn async_executor() -> AsyncExecutor {
    AsyncExecutor::from_u8(ASYNC_EXECUTOR.load(Ordering::Relaxed))
}

/// 是否有 auto 维度尚未校准（任一 auto 无记忆即 true，首启必 true）。
/// 全显式配置返回 false（跳过矩阵，直接起飞）。
/// 注意 workers 只在直写路径有意义：渲染已定死 Blt 时忽略 workers 维度，
/// 否则每次启动都会因它触发无意义的校准。
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
        && measured_workers() == WorkersMode::Auto
    {
        return true;
    }
    // dist 只在“直写 + 全核可能”时有意义：Blt 定死，或 workers 定死 bsp_only
    // （含 auto 已记忆为 bsp_only），都跳过 dist 维度，否则空转。
    let dist_in_play = !render_is_blt
        && workers_mode() != WorkersMode::BspOnly
        && !(workers_mode() == WorkersMode::Auto && measured_workers() == WorkersMode::BspOnly);
    if dist_in_play && dist_mode() == DistMode::Auto && measured_dist() == DistMode::Auto {
        return true;
    }
    // half 自带 bench（冠军计时取半）：无记忆即待测，有记忆直接复用快启。
    if vsync_mode() == VsyncMode::Half && measured_vsync_fps() == 0 {
        return true;
    }
    // Blt 引擎对照操作：选中非 B2V 且 Blt 在测时，单发出数（不参选，只出数）。
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

/// 把记忆/结论写进运行内存（校准完，或复用盘上记忆时调用）。
pub fn set_resolved_workers(w: WorkersMode) {
    RESOLVED_WORKERS.store(w.as_u8(), Ordering::Relaxed);
}
pub fn set_resolved_dist(d: DistMode) {
    RESOLVED_DIST.store(d.as_u8(), Ordering::Relaxed);
}
pub fn set_resolved_vsync(v: VsyncMode) {
    RESOLVED_VSYNC.store(v.as_u8(), Ordering::Relaxed);
}
/// half 定档直写运行结论（冠军 cell 无计时兜底用；正常走 `persist_measured_vsync_fps`）。
pub fn set_resolved_vsync_fps(fps: u32) {
    RESOLVED_VSYNC_FPS.store(fps.max(1), Ordering::Relaxed);
}
/// 复用盘上记忆：把 `measured_*` 装进本启动的结论（跳过矩阵的快启路径）。
pub fn apply_measured_render() {
    if measured_render() != RenderMode::Auto {
        set_resolved_render(measured_render());
    }
    if measured_workers() != WorkersMode::Auto {
        set_resolved_workers(measured_workers());
    }
    if measured_dist() != DistMode::Auto {
        set_resolved_dist(measured_dist());
    }
    if measured_vsync() != VsyncMode::Auto {
        set_resolved_vsync(measured_vsync());
    }
    if measured_vsync_fps() > 0 {
        RESOLVED_VSYNC_FPS.store(measured_vsync_fps(), Ordering::Relaxed);
    }
}

/// 矩阵校准完把结论回写 `\player.toml`（`None` 的维度不动盘上记忆）。
/// 与 `persist_measured_max_fps` 同策略：重读再改，用户手写项不丢。
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

/// 开机解析出的播放帧率开关。
pub fn play_policy() -> FpsPolicy {
    match PLAY_MODE.load(Ordering::Relaxed) {
        1 => FpsPolicy::Capped(PLAY_CAP.load(Ordering::Relaxed).max(1)),
        2 => FpsPolicy::Max,
        _ => FpsPolicy::Auto,
    }
}

/// 已实测的最大播放帧率（0 = 未测）。
pub fn measured_max_fps() -> u32 {
    PLAY_MEASURED.load(Ordering::Relaxed)
}

/// 播片片源格式（默认 qois；raw 读 `1080p/frameNNNN.raw` 序列）。
pub fn play_format() -> MediaFormat {
    MediaFormat::from_u8(PLAY_FORMAT.load(Ordering::Relaxed))
}

/// 播放管线（默认 clip；老文件缺键照读）。
pub fn play_pipeline() -> PlayPipeline {
    PlayPipeline::from_u8(PLAY_PIPELINE.load(Ordering::Relaxed))
}

/// 实测完把最大帧率写回 `\player.toml`（GUI 下次打开即见）。
/// 先重读现文件再改，保证用户手写的 accel/render 设置不丢；
/// 文件不存在就按默认新建。序列化先在内存完成再动盘，写坏不丢原文件。
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

/// half 定档完把预算写回 `\player.toml`（GUI 下次打开即见，同 `persist_measured_max_fps`
/// 策略：重读再改，用户手写项不丢）。同时装进本启动的运行结论。
pub fn persist_measured_vsync_fps(m: u32) -> crate::error::Result {
    MEASURED_VSYNC_FPS.store(m, Ordering::Relaxed);
    RESOLVED_VSYNC_FPS.store(m, Ordering::Relaxed);
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
