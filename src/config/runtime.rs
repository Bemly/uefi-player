//! 播放器运行时快照（`src/config` 的 runtime 半；P2 配置拆分载体）。
//!
//! - `PlayerRuntime`：播放器侧生效值（accel/render/schedule/play/feed/decode/
//!   display/tune/media_dir），字段复用现有配置类型，缺省取各表缺省。
//!   只给 TUI 页面用的契约：字段只可增不可改名，三函数不许改名改签名。
//! - `seed_runtime_from_file()`：开机调一次，从文件拷入，之后脱钩
//!  （运行期只读 `runtime()`，不再碰文件；TUI 经 `runtime_mut()` 改内存）。
//! - 本文件内播放器侧读取函数签名与旧 `mod.rs` 逐字一致，内部改读 runtime；
//!   壁纸/playlist/boot/measured 不在此（见 `wallpaper_io`/`super`）。
//!
//! 线程模型：BSP 单线程 mutation（`runtime_mut` 的 `static mut` 经 BSP 独占保证
//! 安全：开机 `load`/`seed` 单线程装填一次，MP 启动前完成；播放期 AP 只经
//! `runtime()` 共享读，不与 BSP 写并发；TUI 写回亦在 BSP 菜单期，AP 已暂停或
//! 只读快照，见调用方 `mp`/`exec` 快照语义）。校准结论（`RESOLVED_*`/`MAX_ACCEL`）
//! 仍用原子（AP 可读，无需 unsafe）。

use alloc::string::String;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use super::{
    AccelLevel, BarColor, BltOpKind, CopyWidth, DecodeTable, DisplayAlign, DisplayConfig,
    DisplayFit, DistMode, FeedMode, FpsPolicy, RenderMode, RolesMode, ScheduleBackend, VsyncMode,
    WorkersMode, default_buffers, default_frames, default_tile_rows, default_vsync_fps,
    default_window, parse_fps,
};

/// 播放器运行时（TUI 契约，字段只可增不可改名）：
/// - `accel_level`：`[accel] level`（默认 auto）。
/// - `render_mode`/`render_blt_op`：`[render] mode`/`blt_op`（默认 auto/B2V）。
/// - `schedule_*`：`[schedule]` 五键（backend/workers/dist/cores/roles，
///   默认 manual/auto/auto/0/auto；`measured_workers_n` 不在此，仍走 measured）。
/// - `play_fps`/`play_feed`：`[play] fps` 字符串三态（默认 auto）/`feed`
///  （默认 clip）；`media_dir`（P2 新增，`""`=根目录，单层非递归）。
/// - `vsync_mode`/`vsync_fps`：`[render] vsync`（默认 auto）+ `[render.tune] vsync_fps`
///  （默认 60，钳 15-240）。
/// - `tune_*`：`[render.tune]` 四键（frames/tile_rows/buffers/window，
///   默认 30/64/0/4，钳制口径见 `apply_cfg`）。
/// - `display_*`：`[display]` 三键（fit/align/bar，默认 fit/center/black）。
/// - `decode`：`[decode.<fmt>]` 八格式表（沿用现有每表类型，缺表缺键全默认）。
#[derive(Debug, Clone)]
pub struct PlayerRuntime {
    pub accel_level: AccelLevel,
    pub render_mode: RenderMode,
    pub render_blt_op: BltOpKind,
    pub schedule_backend: ScheduleBackend,
    pub schedule_workers: WorkersMode,
    pub schedule_dist: DistMode,
    pub schedule_cores: u32,
    pub schedule_roles: RolesMode,
    pub play_fps: String,
    pub play_feed: FeedMode,
    pub media_dir: String,
    pub vsync_mode: VsyncMode,
    pub vsync_fps: u32,
    pub tune_frames: u32,
    pub tune_tile_rows: u32,
    pub tune_buffers: u32,
    pub tune_window: u32,
    pub display_fit: DisplayFit,
    pub display_align: DisplayAlign,
    pub display_bar: BarColor,
    pub decode: DecodeTable,
}

impl Default for PlayerRuntime {
    fn default() -> Self {
        Self {
            accel_level: AccelLevel::default(),
            render_mode: RenderMode::default(),
            render_blt_op: BltOpKind::default(),
            schedule_backend: ScheduleBackend::default(),
            schedule_workers: WorkersMode::default(),
            schedule_dist: DistMode::default(),
            schedule_cores: 0,
            schedule_roles: RolesMode::default(),
            play_fps: String::from("auto"),
            play_feed: FeedMode::default(),
            media_dir: String::new(),
            vsync_mode: VsyncMode::default(),
            vsync_fps: default_vsync_fps(),
            tune_frames: default_frames(),
            tune_tile_rows: default_tile_rows(),
            tune_buffers: default_buffers(),
            tune_window: default_window(),
            display_fit: DisplayFit::default(),
            display_align: DisplayAlign::default(),
            display_bar: BarColor::default(),
            decode: DecodeTable::default(),
        }
    }
}

// BSP 独占静态（安全理由见模块头；读写经下两函数）。
static mut RUNTIME: PlayerRuntime = PlayerRuntime {
    accel_level: AccelLevel::Auto,
    render_mode: RenderMode::Auto,
    render_blt_op: BltOpKind::BufferToVideo,
    schedule_backend: ScheduleBackend::Manual,
    schedule_workers: WorkersMode::Auto,
    schedule_dist: DistMode::Auto,
    schedule_cores: 0,
    schedule_roles: RolesMode::Auto,
    play_fps: String::new(),
    play_feed: FeedMode::Clip,
    media_dir: String::new(),
    vsync_mode: VsyncMode::Auto,
    vsync_fps: 60,
    tune_frames: 30,
    tune_tile_rows: 64,
    tune_buffers: 0,
    tune_window: 4,
    display_fit: DisplayFit::Fit,
    display_align: DisplayAlign::Center,
    display_bar: BarColor::BLACK,
    decode: DecodeTable {
        qois: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        raw: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        hevc: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        vp9: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        h263: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        mpeg2v: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        theora: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
        avc: super::DecodeEntry {
            threads: super::DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            width: 1920,
            height: 1080,
        },
    },
};

/// 播放器运行时共享读（TUI/播放器共用；BSP 装填后 AP 只读，无写竞争）。
pub fn runtime() -> &'static PlayerRuntime {
    // 安全：装填期 BSP 单线程（`load`/`seed` 在 MP 启动前一次写完），
    // 此后只有 `runtime_mut` 的 BSP 菜单期写与本共享读错开，无并发写-读。
    unsafe { &*(&raw const RUNTIME) }
}

/// 播放器运行时独占写（BSP 单线程 mutation，TUI 页面用）。
/// 安全理由：调用方保证 BSP 独占期（开机装填或菜单暂停期，AP 未跑或只读快照），
/// 不与 `runtime()` 的 AP 并发读重叠；`static mut` 的独占引用由调用契约保证。
pub fn runtime_mut() -> &'static mut PlayerRuntime {
    // 安全：同上，BSP 单线程独占写，调用方保证无 AP 并发读写。
    unsafe { &mut *(&raw mut RUNTIME) }
}

/// 开机调一次，从文件拷入，之后脱钩（运行期只读 runtime，不再碰文件）。
/// 文件缺失/非法即全默认（`parse` 同口径，不 halt）。
pub fn seed_runtime_from_file() {
    let (cfg, _) = super::parse();
    apply_cfg(&cfg);
}

/// 已解析配置拷入 runtime（`load` 与 `seed` 共用；钳制口径与旧 `load` 一致）。
pub(crate) fn apply_cfg(cfg: &super::Config) {
    let r = runtime_mut();
    r.accel_level = cfg.accel.level;
    r.render_mode = cfg.render.mode;
    r.render_blt_op = cfg.render.blt_op;
    r.schedule_backend = cfg.schedule.backend;
    r.schedule_workers = cfg.schedule.workers;
    r.schedule_dist = cfg.schedule.dist;
    r.schedule_cores = cfg.schedule.cores.min(64);
    r.schedule_roles = cfg.schedule.roles;
    r.play_fps = cfg.play.fps.clone();
    r.play_feed = cfg.play.feed;
    r.media_dir = cfg.play.media_dir.clone();
    r.vsync_mode = cfg.render.vsync;
    r.vsync_fps = cfg.render.tune.vsync_fps.clamp(15, 240);
    r.tune_frames = cfg.render.tune.frames.max(1);
    r.tune_tile_rows = cfg.render.tune.tile_rows.clamp(8, 512);
    r.tune_buffers = cfg.render.tune.buffers.min(16);
    r.tune_window = cfg.render.tune.window.clamp(1, 32);
    r.display_fit = cfg.display.fit;
    r.display_align = cfg.display.align;
    r.display_bar = cfg.display.bar;
    // 解码表（buffers 钳 0-32；raw 几何钳 1-4096 + 警告，其余七表残留键已由 serde 忽略）。
    let mut d = cfg.decode;
    d.qois.buffers = d.qois.buffers.min(32);
    d.raw.buffers = d.raw.buffers.min(32);
    d.hevc.buffers = d.hevc.buffers.min(32);
    d.vp9.buffers = d.vp9.buffers.min(32);
    d.h263.buffers = d.h263.buffers.min(32);
    d.mpeg2v.buffers = d.mpeg2v.buffers.min(32);
    d.theora.buffers = d.theora.buffers.min(32);
    d.avc.buffers = d.avc.buffers.min(32);
    let raw_w = d.raw.width.clamp(1, 4096);
    let raw_h = d.raw.height.clamp(1, 4096);
    if raw_w != d.raw.width || raw_h != d.raw.height {
        log::warn!(
            "cfg: [decode.raw] width/height out of range ({}x{}) -- clamped to {}x{}",
            d.raw.width,
            d.raw.height,
            raw_w,
            raw_h,
        );
    }
    d.raw.width = raw_w;
    d.raw.height = raw_h;
    r.decode = d;
}

// ---------- 校准结论（原子；AP 可读） ----------

/// 0=未测，1=direct，2=blt（`mode=auto` 的矩阵结论）。
static RESOLVED_RENDER: AtomicU8 = AtomicU8::new(0);
/// 0=未测，1=bsp_only，2=all（`workers=auto` 的矩阵结论）。
static RESOLVED_WORKERS: AtomicU8 = AtomicU8::new(0);
/// 0=未测，1=static，2=off（`vsync=auto` 的探测结论）。
static RESOLVED_VSYNC: AtomicU8 = AtomicU8::new(0);
/// half 实测预算（fps；0=未测）。
static RESOLVED_VSYNC_FPS: AtomicU32 = AtomicU32::new(0);
/// 0=未测，1=static，2=tile，3=dynamic（`dist=auto` 的矩阵结论）。
static RESOLVED_DIST: AtomicU8 = AtomicU8::new(0);
/// 硬件实测最大搬运位宽（`CopyWidth` 判别值；0=未探，按 X128 保守用）。
static MAX_ACCEL: AtomicU8 = AtomicU8::new(0);

pub fn accel_level() -> AccelLevel {
    runtime().accel_level
}

pub fn render_mode() -> RenderMode {
    runtime().render_mode
}

/// 调度后端（默认 manual；nano/dynamic 走执行器泵，见 `video::exec`）。
pub fn schedule_backend() -> ScheduleBackend {
    runtime().schedule_backend
}

pub fn blt_op() -> BltOpKind {
    runtime().render_blt_op
}

pub fn tune_frames() -> u32 {
    runtime().tune_frames.max(1)
}

/// tile 高（行），已钳制 8-512。
pub fn tune_tile_rows() -> u32 {
    runtime().tune_tile_rows.clamp(8, 512).max(1)
}

/// vsync 预算帧率（`static` 的封顶 + `auto` 探测的达标线），已钳制 15-240。
pub fn tune_vsync_fps() -> u32 {
    runtime().vsync_fps.clamp(15, 240).max(1)
}

/// 后缓冲帧数（0-16，已钳制）：0=直写前台；`>0`=影子环+整推。
pub fn tune_buffers() -> u32 {
    runtime().tune_buffers.min(16)
}

/// 流式供片窗口（已钳制 1-32）。
pub fn tune_window() -> u32 {
    runtime().tune_window.clamp(1, 32).max(1)
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
    runtime().schedule_workers
}

pub fn dist_mode() -> DistMode {
    runtime().schedule_dist
}

pub fn vsync_mode() -> VsyncMode {
    runtime().vsync_mode
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

/// 实际生效的分工：直写+全核才有意义，其余恒 `Tile`。`auto` 用矩阵结论。
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

/// 实际生效的垂直同步：`auto` 用探测结论（未测退回关）。
pub fn effective_vsync() -> VsyncMode {
    match vsync_mode() {
        VsyncMode::Auto => match RESOLVED_VSYNC.load(Ordering::Relaxed) {
            1 => VsyncMode::Static,
            _ => VsyncMode::Off,
        },
        explicit => explicit,
    }
}

/// 实际生效的垂直同步预算（fps）：`half` 取实测取半，其余取滑杆。
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

/// 开机探一次硬件最大搬运位宽（P2）：先 `ensure_xsave_enabled`，再按 CPUID/XCR0 取最高。
/// 必须在任何上屏/bench 之前调（`main` 里调，bench/播放只读）。
/// 末尾把 `[accel]` 开关推进六家解码轮子：单比特跟随全局搬运位宽。
pub fn resolve_accel() {
    #[cfg(target_arch = "x86_64")]
    {
        crate::diag::common::ensure_xsave_enabled();
        let max = probe_max_width();
        MAX_ACCEL.store(max as u8, Ordering::Relaxed);
        let want = accel_level();
        if want != AccelLevel::Auto && level_width(want) as u8 > max as u8 {
            log::warn!(
                "accel: level {:?} beyond hardware ({:?}), clamped",
                want,
                max_width_name(max)
            );
        }
        log::info!("accel: hardware max width = {:?}", max_width_name(max));
        let on = copy_width() != CopyWidth::U64;
        push_hevc_isa(copy_width());
        log::info!("accel: hevc decode isa = {}", rusty_h265::accel::describe());
        let h264_on = matches!(copy_width(), CopyWidth::Y256 | CopyWidth::Z512);
        rusty_h264_accel::set_simd_enabled(h264_on);
        log::info!("accel: h264 decode isa = {}", rusty_h264_accel::describe());
        vp9dec::simd::set_simd_allowed(on);
        h263_rs_yuv::bt601::set_simd_enabled(on);
        h263_rs_deblock::deblock::set_simd_enabled(on);
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

/// 硬件探到的最大搬运位宽（`resolve_accel` 的结论）。
fn hw_max_width() -> CopyWidth {
    CopyWidth::from_u8(MAX_ACCEL.load(Ordering::Relaxed))
}

/// 档位→搬运位宽（位号依据 Intel SDM Vol.2A CPUID，见 `docs/simd.md`）。
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
#[cfg(target_arch = "x86_64")]
fn probe_max_width() -> CopyWidth {
    use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

    let c1 = __cpuid(1);
    let has7 = crate::diag::common::cpuid_max_basic() >= 7;
    let b7 = if has7 { __cpuid_count(7, 0).ebx } else { 0 };
    let ext_ecx = if crate::diag::common::cpuid_max_extended() >= 0x8000_0001 {
        __cpuid(0x8000_0001).ecx
    } else {
        0
    };
    log::info!("accel: ext 80000001h ecx={:#010x}", ext_ecx);
    if has7 && b7 & (1 << 16) != 0 && c1.ecx & (1 << 27) != 0 {
        let xcr0 = unsafe { _xgetbv(0) };
        const ZMM_MASK: u64 = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 7);
        if xcr0 & ZMM_MASK == ZMM_MASK {
            return CopyWidth::Z512;
        }
    }
    if crate::diag::avx_checker::check_avx_support() && has7 && b7 & (1 << 5) != 0 {
        return CopyWidth::Y256;
    }
    if crate::diag::avx_checker::check_avx_support() {
        return CopyWidth::V128;
    }
    if c1.edx & (1 << 26) != 0 {
        return CopyWidth::X128;
    }
    CopyWidth::U64
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
/// half 定档直写运行结论（冠军 cell 无计时兜底用）。
pub fn set_resolved_vsync_fps(fps: u32) {
    RESOLVED_VSYNC_FPS.store(fps.max(1), Ordering::Relaxed);
}
/// 快启复用盘上记忆（`measured_*`→`RESOLVED_*`；`mod.rs` 的
/// `apply_measured_render` 经此装填，保持单源）。
pub(crate) fn apply_measured_inner(
    render: super::RenderMode,
    workers: super::WorkersMode,
    dist: super::DistMode,
    vsync: super::VsyncMode,
    vsync_fps: u32,
) {
    if render != super::RenderMode::Auto {
        set_resolved_render(render);
    }
    if workers != super::WorkersMode::Auto {
        set_resolved_workers(workers);
    }
    if dist != super::DistMode::Auto {
        set_resolved_dist(dist);
    }
    if vsync != super::VsyncMode::Auto {
        set_resolved_vsync(vsync);
    }
    if vsync_fps > 0 {
        RESOLVED_VSYNC_FPS.store(vsync_fps, Ordering::Relaxed);
    }
}

/// 开机解析出的播放帧率开关（读 runtime `play_fps`）。
pub fn play_policy() -> FpsPolicy {
    parse_fps(&runtime().play_fps)
}

/// 供片模式（默认 clip；只切 `Clip` 内部拿帧方式）。
pub fn play_feed() -> FeedMode {
    runtime().play_feed
}

/// 片源目录（P2 新增；`""`=根目录，单层非递归；不存在由扫描侧报错）。
pub fn media_dir() -> String {
    runtime().media_dir.clone()
}

/// 生效 fit（默认 `Fit`）。
pub fn display_fit() -> DisplayFit {
    runtime().display_fit
}

/// 生效 align（默认 `Center`）。
pub fn display_align() -> DisplayAlign {
    runtime().display_align
}

/// 生效 bar（默认 `BLACK`）。
pub fn display_bar() -> BarColor {
    runtime().display_bar
}

/// `[display]` 全表（TUI 一次取三键用）。
pub fn display() -> DisplayConfig {
    DisplayConfig {
        fit: runtime().display_fit,
        align: runtime().display_align,
        bar: runtime().display_bar,
    }
}
