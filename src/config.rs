//! `\player.toml` —— 加速矩阵配置（指令集档位 × 渲染路径）。
//!
//! 位置：启动卷根目录（与本程序写 `HWBENCH*.TXT` 同层）。
//! 解析：`toml` 1.x（no_std）+ serde。缺失/非法 → 全默认 + 屏显一行，不 halt。
//!
//! ```toml
//! version = 1
//! [accel]
//! level = "auto"
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
//! measured_max_fps = 0    # 固件实测后回写，GUI 只读展示
//! [wallpaper]
//! enable = false          # 菜单背景 + 引导过渡总开关（默认关）
//! fps = 15                # 背景循环帧率（低帧即可）
//! frames = 0              # B 过渡封顶帧数（0 = 全播预载集）
//! preload = 8             # A/B 共用预载帧数上限（1080p BGRA ≈ 8MB/帧）
//! [boot]
//! auto = true             # 多卷自动发现总开关（关 = 只列本卷 BLS，老行为）
//! windows = true          # 自动发现 Windows Boot Manager
//! linux = true            # 自动发现 Linux 内核对（kernel + initrd)
//! shell = true            # 自动发现 UEFI Shell（shellx64.efi）
//! uki = true              # 自动发现 EFI/Linux/*.efi（标题取 .uname 区，读不到用文件名）
//! macos = true            # OpenCore 链式接力（EFI/OC/OpenCore.efi 存在即链）
//! strict = false          # 严格模式：BLS 只认本卷 + XBOOTLDR 卷（systemd 严格模式)
//! ```

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use uefi::cstr16;

use crate::error::NyaStatus;
use crate::fs::Fs;

/// 配置文件名（启动卷根目录）。
pub const CONFIG_PATH: &str = "player.toml";

/// 指令集加速档位。`Auto` = 运行时探 CPUID/XCR0 取最高可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccelLevel {
    #[default]
    Auto,
    Scalar,
    Sse2,
    Ssse3,
    Sse41,
    Sse42,
    Avx,
    Avx2,
    Avx512,
}

impl AccelLevel {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Scalar => 1,
            Self::Sse2 => 2,
            Self::Ssse3 => 3,
            Self::Sse41 => 4,
            Self::Sse42 => 5,
            Self::Avx => 6,
            Self::Avx2 => 7,
            Self::Avx512 => 8,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Scalar,
            2 => Self::Sse2,
            3 => Self::Ssse3,
            4 => Self::Sse41,
            5 => Self::Sse42,
            6 => Self::Avx,
            7 => Self::Avx2,
            8 => Self::Avx512,
            _ => Self::Auto,
        }
    }
}

/// 渲染路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// 开机微基准在 direct 与 blt 之间选快的。
    #[default]
    Auto,
    /// CPU 直写 framebuffer。
    Direct,
    /// 走 GOP Blt 引擎。
    Blt,
}

impl RenderMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Direct => 1,
            Self::Blt => 2,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Direct,
            2 => Self::Blt,
            _ => Self::Auto,
        }
    }
}

/// 多核开关：`Auto` = 校准矩阵实测（BSP 单核直写 vs 全核并行），只在
/// `render.mode` 允许直写时有意义（Blt 路径恒单线程，忽略本项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkersMode {
    #[default]
    Auto,
    /// 只用 BSP 单核（关闭 MP 并行）。
    BspOnly,
    /// 全核并行（AP+BSP）。
    All,
}

impl WorkersMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::BspOnly => 1,
            Self::All => 2,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::BspOnly,
            2 => Self::All,
            _ => Self::Auto,
        }
    }
}

/// 帧搬运位宽（P2 真分发的落点）：`accel.level` 只管“档位语义”，
/// 真正上屏/播放走这个位宽。128 位档（SSE2~SSE4.2）同宽，AVX 是 VEX-128。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyWidth {
    U64 = 1,
    X128 = 2,
    V128 = 3,
    Y256 = 4,
    Z512 = 5,
}

impl CopyWidth {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            5 => Self::Z512,
            4 => Self::Y256,
            3 => Self::V128,
            2 | 0 => Self::X128,
            _ => Self::U64,
        }
    }
}

/// 多核分工（P2 调度矩阵）：`Static` = 每核固定行区（零调度开销，同构最优）；
/// `Tile` = 帧切小块（行区），BSP+AP 从原子游标自助领活（异构大小核/负载不均时赢，
/// 顺带把 OSD 行区划出 tile，AP 不再盖 OSD 字）；`Dynamic` = 按实测核速加权分行
/// （快核多拿；校准时 probe 定权计时，播放时每轮按各核上一轮耗时重算，无 tile 游标开销）。
/// `Auto` = 同机实测三选一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DistMode {
    #[default]
    Auto,
    Static,
    Tile,
    Dynamic,
}

impl DistMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Static => 1,
            Self::Tile => 2,
            Self::Dynamic => 3,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Static,
            2 => Self::Tile,
            3 => Self::Dynamic,
            _ => Self::Auto,
        }
    }
}

/// 垂直同步（软件帧预算，GOP 无硬件 vsync）：`Static` = 按 `tune.vsync_fps`
/// 固定封顶；`Half` = 开机实测冠军路最高帧率，取半稳定跑并回写
/// `measured_vsync_fps`（下次启动复用）；`Auto` = 按 `tune.vsync_fps` 预算探测
/// 冠军路，零超期则开（封顶同预算）否则关；`Off` = 只按 `play.fps` 跑。
/// 旧文件的 `"on"` 按 `static` 照读（alias，不断旧配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VsyncMode {
    #[default]
    Auto,
    #[serde(alias = "on")]
    Static,
    Half,
    Off,
}

impl VsyncMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Static => 1,
            Self::Off => 2,
            Self::Half => 3,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Static,
            2 => Self::Off,
            3 => Self::Half,
            _ => Self::Auto,
        }
    }
}

/// `mode = "blt"` 时用哪个 Blt 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BltOpKind {
    #[default]
    BufferToVideo,
    VideoToVideo,
    VideoFill,
    VideoToBltBuffer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccelConfig {
    pub level: AccelLevel,
}

const fn default_frames() -> u32 {
    30
}

const fn default_tile_rows() -> u32 {
    64
}

const fn default_buffers() -> u32 {
    0
}

const fn default_vsync_fps() -> u32 {
    60
}

/// `[render.tune]`：矩阵全部数字旋钮（GUI 逐项可拨，无 auto 维度，改完即生效）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderTune {
    #[serde(default = "default_frames")]
    pub frames: u32,
    /// tile 高（行）：MP 自调度切块粒度，BSP+AP 共用。
    #[serde(default = "default_tile_rows")]
    pub tile_rows: u32,
    /// vsync 预算帧率：`static` 的封顶 + `auto` 探测的达标线。
    /// `half` 不用它（实测取半自带预算），`off` 更不用。
    #[serde(default = "default_vsync_fps")]
    pub vsync_fps: u32,
    /// 后缓冲帧数（0-16，0 = 直写前台现状）：`>0` 时播放/壁纸先画影子再整推，
    /// 把多核散写的撕裂窗口压成一次 push。Blt 路已是单引擎 push，本项无意义。
    /// 显式旋钮（不进矩阵 bench，改它照样清记忆重测）；内存约 N×帧大小，
    /// 分配失败自动退回直写（只记 warning，不断播）。
    #[serde(default = "default_buffers")]
    pub buffers: u32,
}

impl Default for RenderTune {
    fn default() -> Self {
        Self {
            frames: default_frames(),
            tile_rows: default_tile_rows(),
            vsync_fps: default_vsync_fps(),
            buffers: default_buffers(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RenderConfig {
    pub mode: RenderMode,
    pub workers: WorkersMode,
    pub dist: DistMode,
    pub vsync: VsyncMode,
    pub blt_op: BltOpKind,
    pub tune: RenderTune,
    /// auto 校准记忆：`Auto`/`0` = 未测。固件回写，GUI 只读展示 + 重测清零。
    /// 缺键（老配置）按未测处理，下次启动必校准。
    #[serde(default)]
    pub measured_render: RenderMode,
    #[serde(default)]
    pub measured_workers: WorkersMode,
    #[serde(default)]
    pub measured_dist: DistMode,
    #[serde(default)]
    pub measured_vsync: VsyncMode,
    /// half 实测预算（fps；0 = 未测）：`vsync=half` 首启现测取半后回写，下次复用。
    /// 缺键（老配置）按未测处理。`static`/`auto`/`off` 不用它。
    #[serde(default)]
    pub measured_vsync_fps: u32,
    /// 校准时机型 AP+BSP 总数（0 = 未测，供矩阵归因/复核）。
    #[serde(default)]
    pub measured_cores: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub accel: AccelConfig,
    pub render: RenderConfig,
    pub play: PlayConfig,
    pub wallpaper: WallpaperConfig,
    pub boot: BootConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            accel: AccelConfig::default(),
            render: RenderConfig::default(),
            play: PlayConfig::default(),
            wallpaper: WallpaperConfig::default(),
            boot: BootConfig::default(),
        }
    }
}

/// `[boot]`：多卷自动发现开关（P1 全功能对齐）。本卷 BLS 永远列（不受开关影响，
/// 老行为）；`strict` = BLS 只认本卷 + XBOOTLDR 卷（systemd 严格模式）。
/// `macos` = OpenCore 链式接力（`\EFI\OC\OpenCore.efi` 存在即链；不扫通用
/// `BOOTX64.EFI`，那可能是我们自己，会自举死循环；macOS 原生引导不做，
/// 要 bless/APFS，超出 UEFI 范围）。
/// 缺键老配置按默认（全开、不严格）处理。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct BootConfig {
    #[serde(default = "default_true")]
    pub auto: bool,
    #[serde(default = "default_true")]
    pub windows: bool,
    #[serde(default = "default_true")]
    pub linux: bool,
    #[serde(default = "default_true")]
    pub shell: bool,
    #[serde(default = "default_true")]
    pub uki: bool,
    #[serde(default = "default_true")]
    pub macos: bool,
    #[serde(default)]
    pub strict: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            auto: true,
            windows: true,
            linux: true,
            shell: true,
            uki: true,
            macos: true,
            strict: false,
        }
    }
}

const fn default_wallpaper_fps() -> u32 {
    15
}

const fn default_wallpaper_preload() -> u32 {
    8
}

/// `[wallpaper]`：动态壁纸开关（P4）。序列位图 `wallpaper/frameNNNN.raw`
///（BGRA 直存，首版定死 1920x1080），缺键老配置按默认（关）处理。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct WallpaperConfig {
    pub enable: bool,
    #[serde(default = "default_wallpaper_fps")]
    pub fps: u32,
    /// B 过渡封顶帧数（0 = 全播预载集）。
    #[serde(default)]
    pub frames: u32,
    /// 预载帧数上限（内存兜底；单帧 1920x1080x4 ≈ 8MB）。
    #[serde(default = "default_wallpaper_preload")]
    pub preload: u32,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            enable: false,
            fps: default_wallpaper_fps(),
            frames: 0,
            preload: default_wallpaper_preload(),
        }
    }
}

fn default_fps() -> String {
    String::from("auto")
}

/// 播放帧率开关（GUI 滑杆：最左 auto / 中间 1-240 / 最右 max）。
/// - `Auto`：首启实测最大播放帧率并回写，按 `min(视频帧率, 实测*3/4)` 跑；
/// - `Max`：不限速；
/// - `Capped(n)`：显式限速。非法值回落 `Auto`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpsPolicy {
    Auto,
    Max,
    Capped(u32),
}

pub fn parse_fps(s: &str) -> FpsPolicy {
    match s.trim().to_ascii_lowercase().as_str() {
        "max" => FpsPolicy::Max,
        "auto" | "" => FpsPolicy::Auto,
        n => n
            .parse::<u32>()
            .map(|v| FpsPolicy::Capped(v.clamp(1, 240)))
            .unwrap_or(FpsPolicy::Auto),
    }
}

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），GUI 只读展示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayConfig {
    #[serde(default = "default_fps")]
    pub fps: String,
    #[serde(default)]
    pub measured_max_fps: u32,
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            measured_max_fps: 0,
        }
    }
}

/// 解析结果：配置 + 可选的屏显警告（文件不存在不算错，返回 None）。
pub fn parse() -> (Config, Option<String>) {
    let mut fs = match Fs::new() {
        Ok(f) => f,
        Err(e) => return (Config::default(), Some(format!("cfg: open volume failed: {:?}", e))),
    };

    let bytes = match fs.read_file(cstr16!("player.toml")) {
        Ok(b) => b,
        // 没有配置文件是正常情况：静默用默认。
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            return (Config::default(), None)
        }
        Err(e) => return (Config::default(), Some(format!("cfg: read failed: {:?}", e))),
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
static WALLPAPER_FPS: AtomicU32 = AtomicU32::new(default_wallpaper_fps());
static WALLPAPER_FRAMES: AtomicU32 = AtomicU32::new(0);
static WALLPAPER_PRELOAD: AtomicU32 = AtomicU32::new(default_wallpaper_preload());
/// `[boot]` 自动发现开关（5 个 u8，0/1）。
static BOOT_AUTO: AtomicU8 = AtomicU8::new(1);
static BOOT_WINDOWS: AtomicU8 = AtomicU8::new(1);
static BOOT_LINUX: AtomicU8 = AtomicU8::new(1);
static BOOT_SHELL: AtomicU8 = AtomicU8::new(1);
static BOOT_UKI: AtomicU8 = AtomicU8::new(1);
static BOOT_MACOS: AtomicU8 = AtomicU8::new(1);
static BOOT_STRICT: AtomicU8 = AtomicU8::new(0);
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
    let (cfg, warn) = parse();
    ACCEL.store(cfg.accel.level.as_u8(), Ordering::Relaxed);
    RENDER.store(cfg.render.mode.as_u8(), Ordering::Relaxed);
    WORKERS.store(cfg.render.workers.as_u8(), Ordering::Relaxed);
    DIST.store(cfg.render.dist.as_u8(), Ordering::Relaxed);
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
    WALLPAPER_ENABLE.store(cfg.wallpaper.enable as u8, Ordering::Relaxed);
    WALLPAPER_FPS.store(cfg.wallpaper.fps.clamp(1, 60).max(1), Ordering::Relaxed);
    WALLPAPER_FRAMES.store(cfg.wallpaper.frames, Ordering::Relaxed);
    WALLPAPER_PRELOAD.store(cfg.wallpaper.preload.clamp(1, 32).max(1), Ordering::Relaxed);
    BOOT_AUTO.store(cfg.boot.auto as u8, Ordering::Relaxed);
    BOOT_WINDOWS.store(cfg.boot.windows as u8, Ordering::Relaxed);
    BOOT_LINUX.store(cfg.boot.linux as u8, Ordering::Relaxed);
    BOOT_SHELL.store(cfg.boot.shell as u8, Ordering::Relaxed);
    BOOT_UKI.store(cfg.boot.uki as u8, Ordering::Relaxed);
    BOOT_MACOS.store(cfg.boot.macos as u8, Ordering::Relaxed);
    BOOT_STRICT.store(cfg.boot.strict as u8, Ordering::Relaxed);
    log::info!(
        "cfg: version={} accel.level={:?} render.mode={:?} workers={:?} dist={:?} vsync={:?} blt_op={:?} tune.frames={} measured(render={:?} workers={:?} dist={:?} vsync={:?} vhalf={} cores={}) play.fps={} measured_max_fps={} boot(auto={} win={} lin={} sh={} uki={} mac={} strict={})",
        cfg.version,
        cfg.accel.level,
        cfg.render.mode,
        cfg.render.workers,
        cfg.render.dist,
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
        cfg.play.measured_max_fps,
        cfg.boot.auto,
        cfg.boot.windows,
        cfg.boot.linux,
        cfg.boot.shell,
        cfg.boot.uki,
        cfg.boot.macos,
        cfg.boot.strict,
    );
    warn
}

pub fn accel_level() -> AccelLevel {
    AccelLevel::from_u8(ACCEL.load(Ordering::Relaxed))
}

pub fn render_mode() -> RenderMode {
    RenderMode::from_u8(RENDER.load(Ordering::Relaxed))
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
    }
}

/// 实际生效的搬运位宽：`auto` 取硬件最大；显式档位钳到硬件最大。
pub fn copy_width() -> CopyWidth {
    let max = CopyWidth::from_u8(MAX_ACCEL.load(Ordering::Relaxed));
    let want = match accel_level() {
        AccelLevel::Auto => max,
        explicit => level_width(explicit),
    };
    if want as u8 > max as u8 { max } else { want }
}

fn level_width(l: AccelLevel) -> CopyWidth {
    match l {
        AccelLevel::Auto => CopyWidth::X128,
        AccelLevel::Scalar => CopyWidth::U64,
        AccelLevel::Sse2 | AccelLevel::Ssse3 | AccelLevel::Sse41 | AccelLevel::Sse42 => {
            CopyWidth::X128
        }
        AccelLevel::Avx => CopyWidth::V128,
        AccelLevel::Avx2 => CopyWidth::Y256,
        AccelLevel::Avx512 => CopyWidth::Z512,
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
#[cfg(target_arch = "x86_64")]
fn probe_max_width() -> CopyWidth {
    use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

    let c1 = __cpuid(1);
    let has7 = crate::diag::common::cpuid_max_basic() >= 7;
    let b7 = if has7 { __cpuid_count(7, 0).ebx } else { 0 };
    // AVX512：硬件位 + OSXSAVE + XCR0 五位全开，缺一个执行就 #UD。
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
/// 壁纸循环帧率（1-60，已钳制）。
pub fn wallpaper_fps() -> u32 {
    WALLPAPER_FPS.load(Ordering::Relaxed).max(1)
}
/// B 过渡封顶帧数（0 = 全播预载集）。
pub fn wallpaper_frames() -> u32 {
    WALLPAPER_FRAMES.load(Ordering::Relaxed)
}
/// 预载帧数上限（1-32，已钳制；单帧约 8MB，内存兜底）。
pub fn wallpaper_preload() -> u32 {
    WALLPAPER_PRELOAD.load(Ordering::Relaxed).max(1)
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
