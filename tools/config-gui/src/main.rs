//! `player.toml` 跨平台可视化编辑器（host 工具，非 UEFI）。
//!
//! 轮子选择（结论，见根 `AGENTS.md`「轮子优先」）：
//! - 搜过 `crates.io`：“egui glass blur”只有 `egui_glass`（Apple Liquid Glass，
//!   GPU 经 egui-wgpu 渲染）是现货，其余多为整窗 vibrancy（`window-vibrancy`）
//!   或已停更的 css 式皮肤，不满足“任意面板玻璃拟态”，故直接用它，不自研模糊管线。
//! - 搜过文件对话框：“egui file dialog”多为手写 picker；`rfd` 是原生对话框的
//!   事实标准轮子，故用 `rfd`，不手写。
//! - 搜过 i18n：“rust i18n”里 `rust-i18n`（400 万下载，YAML 编译期进二进制，
//!   `t!` + `set_locale`）最轻；`fluent` 要 unic-langid + ftl 文件，杀鸡用牛刀，
//!   故用前者。文案全在 `locales/`，中英切换只是 `set_locale`。
//! - 搜过 CJK 字体：egui 默认字体无中文字形，系统字体各平台 ttc 路径不一且
//!   egui 只取合集第 0 个字重；`egui-chinese-font` 是调系统字体的 std 轮，
//!   免打包但吃整套系统字库。最终用缝合像素 fusion-pixel 10px proportional
//!   简中 TTF 全量文件（真 `glyf` 轮廓，skrifa 可吃；`ms.bitmap/otb/pcf` 纯位图的不行）：
//!   `assets/fusion-pixel-10px-proportional-zh_hans.ttf`（上游 v2026.10.16 原包，
//!   `include_bytes!` 编进二进制，OFL-1.1 见 `assets/OFL.txt`），放字体链首位，
//!   全 UI 像素风，西文 fallback 到默认字体。8x8 中文 Rust 轮子无现货
//!   （font8x8 无中文），不找了。
//!   注意：字体禁止子集化（AGENTS 有令），缺字直接用全量，体积换完整。
//! - 驱动：`eframe` 的 `wgpu` 特性即 wgpu 驱动；`egui_glass` 不支持 glow，
//!   故 `NativeOptions.renderer` 强制 `Wgpu`.
//! - 图标：用户给的复古像素显示器图，按内容框裁掉下边框留白后透明垫正方形
//!   （`assets/icon.png` 1024 主图）；`icon-256.rgba` 预烘焙进二进制做运行时窗口图标
//!   （零新依赖，mac Dock 走 bundle 的 `icon.icns`，见 `bundle_macos.sh`），
//!   `icon.ico` 给 Windows 用。重做图标跑 `make_icon.py <原图>` + `iconutil` 即可。
//!
//! UI 布局照抄 `egui_glass` 官方 README 配方：“Sidebar and opaque content”
//! （顶栏胶囊 + 底栏状态胶囊 → 左侧玻璃导航卡 →
//! 中央大号玻璃薄片放稠密表单），分隔一律留白，不用 `Separator`。
//! 试错记录：曾因 Panel 边界直线误伤过胶囊/薄片，真凶是 `Panel` 默认的
//! `show_separator_line`（已全关），详见 AGENTS.md。
//!
//! 实时背景（`LiveBackdrop`，用户拍板）：页面层是只读摘要文档（跑两遍，
//! 禁写状态、禁玻璃件），开关浮层在上折射它。静态底只留作首帧兜底。
//! 代价是每帧多一次排版 + 离屏渲染 + mipmap，本工具页面小，可接受。
//!
//! 全玻璃强制要求（用户拍板）：所有可点击控件一律用库内组件——`GlassButton`
//! 做动作按钮/导航/单选开关，`GlassSlider` 做滑杆，`Glass`/`GlassToolbar` 做容器。
//! 注意这盖过了官方“玻璃不叠玻璃”（GlassButton 不嵌 Glass/Toolbar）的建议：
//! 嵌套处的折射是同一张 backdrop，多一层 tint，在本工具这种浅层 UI 上可接受。
//! 唯一的例外是无 GPU 兜底分支——玻璃管线没注册，只能用普通 egui 控件。
//!
//! 与固件侧 `src/config.rs` 同 schema（字段名/枚举字符串一致），改完存回 U 盘根目录即可.

use eframe::egui;
use rust_i18n::t;
use serde::{Deserialize, Serialize};

rust_i18n::i18n!("locales", fallback = "en");

// ---------- 与固件 config.rs 同构的 schema（改动必须双边同步） ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AccelLevel {
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
    const ALL: [Self; 9] = [
        Self::Auto,
        Self::Scalar,
        Self::Sse2,
        Self::Ssse3,
        Self::Sse41,
        Self::Sse42,
        Self::Avx,
        Self::Avx2,
        Self::Avx512,
    ];
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scalar => "scalar",
            Self::Sse2 => "sse2",
            Self::Ssse3 => "ssse3",
            Self::Sse41 => "sse41",
            Self::Sse42 => "sse42",
            Self::Avx => "avx",
            Self::Avx2 => "avx2",
            Self::Avx512 => "avx512",
        }
    }
    /// 滑杆档位 0-8（`ALL` 的下标，`Auto=0`）。
    fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RenderMode {
    #[default]
    Auto,
    Direct,
    Blt,
}

impl RenderMode {
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Direct => "direct",
            Self::Blt => "blt",
        }
    }
}

/// 多核开关（与固件 `WorkersMode` 同构；Blt 路径恒单线程，忽略本项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkersMode {
    #[default]
    Auto,
    BspOnly,
    All,
}

impl WorkersMode {
    const ALL: [Self; 3] = [Self::Auto, Self::BspOnly, Self::All];
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::BspOnly => "bsp_only",
            Self::All => "all",
        }
    }
}

/// 多核分工（与固件 `DistMode` 同构；`Tile` = tile 自调度，`Static` = 固定分区，
/// `Dynamic` = 按实测核速加权分行，播中逐轮重算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DistMode {
    #[default]
    Auto,
    Static,
    Tile,
    Dynamic,
}

impl DistMode {
    const ALL: [Self; 4] = [Self::Auto, Self::Static, Self::Tile, Self::Dynamic];
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Static => "static",
            Self::Tile => "tile",
            Self::Dynamic => "dynamic",
        }
    }
}

/// 垂直同步（与固件 `VsyncMode` 同构；`Static` 按预算滑杆封顶，`Half` 实测取半，
/// `Off` 只按播放帧率跑；旧文件 `"on"` 按 `static` 照读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VsyncMode {
    #[default]
    Auto,
    #[serde(alias = "on")]
    Static,
    Half,
    Off,
}

impl VsyncMode {
    const ALL: [Self; 4] = [Self::Auto, Self::Static, Self::Half, Self::Off];
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Static => "static",
            Self::Half => "half",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BltOpKind {
    #[default]
    BufferToVideo,
    VideoToVideo,
    VideoFill,
    VideoToBltBuffer,
}

impl BltOpKind {
    const ALL: [Self; 4] = [
        Self::BufferToVideo,
        Self::VideoToVideo,
        Self::VideoFill,
        Self::VideoToBltBuffer,
    ];
    fn file_str(self) -> &'static str {
        match self {
            Self::BufferToVideo => "buffer_to_video",
            Self::VideoToVideo => "video_to_video",
            Self::VideoFill => "video_fill",
            Self::VideoToBltBuffer => "video_to_blt_buffer",
        }
    }
}

fn default_frames() -> u32 {
    30
}

fn default_fps() -> String {
    String::from("auto")
}

fn default_wallpaper_fps() -> u32 {
    15
}

fn default_wallpaper_preload() -> u32 {
    8
}

fn default_tile_rows() -> u32 {
    64
}

fn default_vsync_fps() -> u32 {
    60
}

fn default_buffers() -> u32 {
    0
}

/// 播放帧率开关（与固件 `parse_fps` 同逻辑：非法值回落 auto）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FpsPolicy {
    Auto,
    Max,
    Capped(u32),
}

fn parse_fps(s: &str) -> FpsPolicy {
    match s.trim().to_ascii_lowercase().as_str() {
        "max" => FpsPolicy::Max,
        "auto" | "" => FpsPolicy::Auto,
        n => n
            .parse::<u32>()
            .map(|v| FpsPolicy::Capped(v.clamp(1, 240)))
            .unwrap_or(FpsPolicy::Auto),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct AccelConfig {
    level: AccelLevel,
}

impl Default for AccelConfig {
    fn default() -> Self {
        Self {
            level: AccelLevel::Auto,
        }
    }
}

/// `[render.tune]`：矩阵全部数字旋钮（无 auto 维度，改完存盘即生效）。
/// `buffers` 是显式后缓冲开关（0 = 直写，1-16 = 影子环 + 整推），不进矩阵 bench，
/// 但改它照样清记忆重测（出屏路径变了，旧结论作废）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RenderTune {
    #[serde(default = "default_frames")]
    frames: u32,
    #[serde(default = "default_tile_rows")]
    tile_rows: u32,
    #[serde(default = "default_vsync_fps")]
    vsync_fps: u32,
    #[serde(default = "default_buffers")]
    buffers: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
struct RenderConfig {
    mode: RenderMode,
    workers: WorkersMode,
    dist: DistMode,
    vsync: VsyncMode,
    blt_op: BltOpKind,
    tune: RenderTune,
    /// auto 校准记忆（`Auto`/`0` = 未测）：只由固件回写，这里只读展示 + 重测清零。
    #[serde(default)]
    measured_render: RenderMode,
    #[serde(default)]
    measured_workers: WorkersMode,
    #[serde(default)]
    measured_dist: DistMode,
    #[serde(default)]
    measured_vsync: VsyncMode,
    /// half 实测预算（0 = 未测）：固件回写，只读展示 + 重测清零。
    #[serde(default)]
    measured_vsync_fps: u32,
    #[serde(default)]
    measured_cores: u32,
}

/// 片源格式（与固件 `MediaFormat` 同构；小写序列化）。
/// 缺键老配置：play 按 qois、wallpaper 按 raw（见两侧 `default_*_format`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MediaFormat {
    Qois,
    Raw,
}

impl MediaFormat {
    fn file_str(self) -> &'static str {
        match self {
            Self::Qois => "qois",
            Self::Raw => "raw",
        }
    }
}

fn default_play_format() -> MediaFormat {
    MediaFormat::Qois
}

fn default_wallpaper_format() -> MediaFormat {
    MediaFormat::Raw
}

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），这里只读展示 + 重测清零。
/// `format` 切片源：qois 读 `1080p/video.qois`，raw 读 `1080p/frameNNNN.raw`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct PlayConfig {
    #[serde(default = "default_fps")]
    fps: String,
    #[serde(default)]
    measured_max_fps: u32,
    #[serde(default = "default_play_format")]
    format: MediaFormat,
}

/// `[boot]`：多卷自动发现开关（与固件 `BootConfig` 同构；全 `true` = 默认全开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct BootConfig {
    auto: bool,
    windows: bool,
    linux: bool,
    shell: bool,
    uki: bool,
    macos: bool,
    strict: bool,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self { auto: true, windows: true, linux: true, shell: true, uki: true, macos: true, strict: false }
    }
}

/// `[wallpaper]`：与固件 `WallpaperConfig` 同构。无校准记忆，纯开关。
/// `format` 切片源：raw 读 `wallpaper/frameNNNN.raw`，qois 读 `wallpaper/wall.qois`。
/// `popup_freeze`：选项弹窗时冻结壁纸（false 则行区继续播）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct WallpaperConfig {
    enable: bool,
    #[serde(default = "default_wallpaper_fps")]
    fps: u32,
    #[serde(default)]
    frames: u32,
    #[serde(default = "default_wallpaper_preload")]
    preload: u32,
    #[serde(default = "default_wallpaper_format")]
    format: MediaFormat,
    #[serde(default = "default_true")]
    popup_freeze: bool,
}

fn default_true() -> bool {
    true
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            enable: false,
            fps: default_wallpaper_fps(),
            frames: 0,
            preload: default_wallpaper_preload(),
            format: MediaFormat::Raw,
            popup_freeze: true,
        }
    }
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            measured_max_fps: 0,
            format: MediaFormat::Qois,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct FileConfig {
    version: u32,
    accel: AccelConfig,
    render: RenderConfig,
    play: PlayConfig,
    wallpaper: WallpaperConfig,
    boot: BootConfig,
}

impl Default for FileConfig {
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

// ---------- App ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    File,
    Accel,
    Wallpaper,
    Boot,
    Summary,
}

/// 开关旁的一句话说明。`t!` 只要字面量 key，这里按档位 match（返回 String 省 lifetime 纠缠）。
fn level_desc(v: AccelLevel) -> String {
    match v {
        AccelLevel::Auto => t!("accel.desc_auto"),
        AccelLevel::Scalar => t!("accel.desc_scalar"),
        AccelLevel::Sse2 => t!("accel.desc_sse2"),
        AccelLevel::Ssse3 => t!("accel.desc_ssse3"),
        AccelLevel::Sse41 => t!("accel.desc_sse41"),
        AccelLevel::Sse42 => t!("accel.desc_sse42"),
        AccelLevel::Avx => t!("accel.desc_avx"),
        AccelLevel::Avx2 => t!("accel.desc_avx2"),
        AccelLevel::Avx512 => t!("accel.desc_avx512"),
    }
    .into_owned()
}

fn mode_desc(v: RenderMode) -> String {
    match v {
        RenderMode::Auto => t!("accel.desc_mode_auto"),
        RenderMode::Direct => t!("accel.desc_mode_direct"),
        RenderMode::Blt => t!("accel.desc_mode_blt"),
    }
    .into_owned()
}

fn wp_enable_desc(on: bool) -> String {
    if on {
        t!("wallpaper.desc_on")
    } else {
        t!("wallpaper.desc_off")
    }
    .into_owned()
}

fn blt_desc(v: BltOpKind) -> String {
    match v {
        BltOpKind::BufferToVideo => t!("accel.desc_b2v"),
        BltOpKind::VideoToVideo => t!("accel.desc_v2v"),
        BltOpKind::VideoFill => t!("accel.desc_fill"),
        BltOpKind::VideoToBltBuffer => t!("accel.desc_v2b"),
    }
    .into_owned()
}

fn workers_desc(v: WorkersMode) -> String {
    match v {
        WorkersMode::Auto => t!("accel.desc_workers_auto"),
        WorkersMode::BspOnly => t!("accel.desc_workers_bsp"),
        WorkersMode::All => t!("accel.desc_workers_all"),
    }
    .into_owned()
}

fn dist_desc(v: DistMode) -> String {
    match v {
        DistMode::Auto => t!("accel.desc_dist_auto"),
        DistMode::Static => t!("accel.desc_dist_static"),
        DistMode::Tile => t!("accel.desc_dist_tile"),
        DistMode::Dynamic => t!("accel.desc_dist_dynamic"),
    }
    .into_owned()
}

fn vsync_desc(v: VsyncMode) -> String {
    match v {
        VsyncMode::Auto => t!("accel.desc_vsync_auto"),
        VsyncMode::Static => t!("accel.desc_vsync_static"),
        VsyncMode::Half => t!("accel.desc_vsync_half"),
        VsyncMode::Off => t!("accel.desc_vsync_off"),
    }
    .into_owned()
}

/// 黑盒矩阵可测的纯规则（无 display 依赖；`App` 方法只是薄委托，UI 照此摆）。
/// 新增 GUI 规则时先加这里 + 矩阵表，再接 UI（见 AGENTS「黑盒矩阵测试」）。
/// 微调区四旋钮显隐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TuneVisible {
    frames: bool,
    tile: bool,
    vfps: bool,
    buf: bool,
}

/// 开机矩阵待测（与固件 `config::needs_render_bench` 同逻辑 + half）：
/// Blt 下 workers/dist 不参测，单核下 dist 不参测。
fn bench_pending(m: &RenderConfig) -> bool {
    if m.mode == RenderMode::Auto && m.measured_render == RenderMode::Auto {
        return true;
    }
    let render_is_blt = match m.mode {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => m.measured_render == RenderMode::Blt,
    };
    if !render_is_blt
        && m.workers == WorkersMode::Auto
        && m.measured_workers == WorkersMode::Auto
    {
        return true;
    }
    let dist_in_play = !render_is_blt
        && m.workers != WorkersMode::BspOnly
        && !(m.workers == WorkersMode::Auto && m.measured_workers == WorkersMode::BspOnly);
    if dist_in_play && m.dist == DistMode::Auto && m.measured_dist == DistMode::Auto {
        return true;
    }
    // half 自带 bench（冠军计时取半）：无记忆即待测。
    if m.vsync == VsyncMode::Half && m.measured_vsync_fps == 0 {
        return true;
    }
    // Blt 引擎对照操作：选中非 B2V 且 Blt 在测时，单发出数（不参选，只出数）。
    let blt_in_play = match m.mode {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => {
            m.measured_render == RenderMode::Auto || m.measured_render == RenderMode::Blt
        }
    };
    if blt_in_play && m.blt_op != BltOpKind::BufferToVideo {
        return true;
    }
    m.vsync == VsyncMode::Auto && m.measured_vsync == VsyncMode::Auto
}

/// 微调区显隐：frames=矩阵有待测；tile=直写全核且分工沾 tile；
/// vfps=static 封顶/auto 达标线；buf=非 Blt（Blt 已是单 push）。
fn tune_visible(m: &RenderConfig) -> TuneVisible {
    let dist_in_play = m.mode != RenderMode::Blt && m.workers != WorkersMode::BspOnly;
    TuneVisible {
        frames: bench_pending(m),
        tile: dist_in_play && (m.dist == DistMode::Auto || m.dist == DistMode::Tile),
        vfps: m.vsync == VsyncMode::Auto || m.vsync == VsyncMode::Static,
        buf: m.mode != RenderMode::Blt,
    }
}

/// 读盘钳制：Blt 三个非上屏操作 GUI 置灰不可选，残留值钳回 B2V
/// （播放行为无变化——固件上屏路本来就只认 B2V；手写值想出引擎数
/// 的改完别点保存，或固件侧照读，见 player.md）。
fn coerce_loaded(mut cfg: FileConfig) -> FileConfig {
    if cfg.render.blt_op != BltOpKind::BufferToVideo {
        cfg.render.blt_op = BltOpKind::BufferToVideo;
    }
    cfg
}

/// 落盘前钳制 tune（滑杆已钳，这里防手改文件后的脏值回写）。
fn clamp_tune(t: &mut RenderTune) {
    t.frames = t.frames.max(1);
    t.tile_rows = t.tile_rows.clamp(8, 512);
    t.vsync_fps = t.vsync_fps.clamp(15, 240);
    t.buffers = t.buffers.min(16);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    Open,
    Save,
    SaveAs,
    Reset,
    ToggleTheme,
    ToggleLang,
}

struct App {
    path: String,
    cfg: FileConfig,
    status: String,
    page: Page,
    /// 播放帧率滑杆镜像：0 = auto，1-240 = 限速，241 = max。
    /// `GlassSlider` 只要 `f32`，每帧写回 `cfg.play.fps`。
    play_f: f32,
    /// 加速档位滑杆镜像：0-8 对应 `AccelLevel::ALL` 下标。
    accel_f: f32,
    /// 壁纸滑杆镜像：fps 1-30，frames 0-120（0 = 全播），preload 1-16。
    /// 库内滑杆只吃 `f32`，整数档用 round 镜像（见 Accel 页 `*_f` 字段）。
    wp_fps_f: f32,
    wp_frames_f: f32,
    wp_preload_f: f32,
    /// 微调滑杆镜像：frames 1-120，tile_rows 8-512（步进 8），vsync_fps 15-240，
    /// buffers 0-16（0 = 直写，步进 1）。
    tune_frames_f: f32,
    tune_tile_f: f32,
    tune_vsync_fps_f: f32,
    tune_buffers_f: f32,
    /// 存盘时对比：tune 变了就清空全部实测记忆（bench 条件变了，旧结论作废，
    /// 下次启动重测；否则改 tune 永不生效——记忆命中直接跳过矩阵）。
    tune_at_load: RenderTune,
    glass: bool,
    /// 深色模式：egui visuals + 纯黑 backdrop + 深色玻璃预设三处联动。
    dark: bool,
    /// 中文模式：`rust_i18n::set_locale` 全局切换，文案全走 `t!`。
    zh: bool,
    /// 存一份 render state，深浅切换时重传 backdrop 用（`RenderState: Clone`）。
    rs: Option<egui_wgpu::RenderState>,
    /// 实时背景：页面跑两遍（可见 + 离屏折射源），开关浮在上层。无 GPU 时为 None。
    live: Option<egui_glass::LiveBackdrop>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 缝合像素 10px 简中全量优先（西文缺字形会 fallback 到默认字体）。
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "fusion".to_owned(),
            egui::FontData::from_static(include_bytes!(
                "../assets/fusion-pixel-10px-proportional-zh_hans.ttf"
            ))
            .into(),
        );
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            fam.insert(0, "fusion".to_owned());
        }
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            fam.insert(0, "fusion".to_owned());
        }
        // 双胞胎上下文用同一套字体（含 cjk），离屏排版才和可见一致。
        let live_fonts = fonts.clone();
        cc.egui_ctx.set_fonts(fonts);

        // 默认英文 + 浅色（白底），与初始 backdrop 一致。
        // 窗口标题跟语言走（中英切换时见 ToggleLang），首帧即按当前 locale 下发。
        rust_i18n::set_locale("en");
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut glass = false;
        let mut rs = None;
        let mut live = None;
        if let Some(r) = cc.wgpu_render_state.as_ref() {
            // MSAA 数与 NativeOptions 一致（默认不开 MSAA，传 1）。
            egui_glass::init(r, 1);
            // 顺序按官方要求：init → live → set_backdrop（静态底只当兜底，
            // live 每帧都跑时实际采的是离屏页面）。
            live = Some(egui_glass::LiveBackdrop::new(r, Some(live_fonts)));
            egui_glass::set_backdrop(&cc.egui_ctx, r, &make_backdrop(false));
            glass = true;
            rs = Some(r.clone());
        }
        let mut app = Self {
            path: "player.toml".to_owned(),
            cfg: FileConfig::default(),
            status: t!("status.ready").into_owned(),
            page: Page::File,
            play_f: 0.0,
            accel_f: AccelLevel::Auto.index() as f32,
            wp_fps_f: default_wallpaper_fps() as f32,
            wp_frames_f: 0.0,
            wp_preload_f: default_wallpaper_preload() as f32,
            tune_frames_f: default_frames() as f32,
            tune_tile_f: default_tile_rows() as f32,
            tune_vsync_fps_f: default_vsync_fps() as f32,
            tune_buffers_f: default_buffers() as f32,
            tune_at_load: RenderTune::default(),
            glass,
            dark: false,
            zh: false,
            rs,
            live,
        };
        // 启动时若工作目录下已有 player.toml，直接载入。
        if std::path::Path::new(&app.path).exists() {
            let p = app.path.clone();
            app.load_path(p);
        }
        app
    }

    fn sync_widgets(&mut self) {
        self.play_f = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => 0.0,
            FpsPolicy::Max => 241.0,
            FpsPolicy::Capped(n) => n as f32,
        };
        self.wp_fps_f = self.cfg.wallpaper.fps.clamp(1, 30) as f32;
        self.wp_frames_f = self.cfg.wallpaper.frames.min(120) as f32;
        self.wp_preload_f = self.cfg.wallpaper.preload.clamp(1, 16) as f32;
        self.tune_frames_f = self.cfg.render.tune.frames.clamp(1, 120) as f32;
        self.tune_tile_f = self.cfg.render.tune.tile_rows.clamp(8, 512) as f32;
        self.tune_vsync_fps_f = self.cfg.render.tune.vsync_fps.clamp(15, 240) as f32;
        self.tune_buffers_f = self.cfg.render.tune.buffers.min(16) as f32;
    }

    /// 滑杆/摘要共用的档位文本。
    fn play_text(&self) -> String {
        let base = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => t!("play.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n),
        };
        format!("{} {}", base, self.cfg.play.format.file_str())
    }

    /// 片源格式行（两颗玻璃按钮，qois/raw 二选一，壁纸/播放共用形状）。
    fn format_row(&mut self, ui: &mut egui::Ui, cur: MediaFormat) -> MediaFormat {
        let mut next = cur;
        ui.horizontal(|ui| {
            if self.option_button(ui, cur == MediaFormat::Qois, &t!("format.qois")) {
                next = MediaFormat::Qois;
            }
            if self.option_button(ui, cur == MediaFormat::Raw, &t!("format.raw")) {
                next = MediaFormat::Raw;
            }
        });
        next
    }

    /// 实测行：有值报数，无值提示切 auto 下次启动测。
    /// `&self`：背景纸（`page_backdrop`）只读重排也能调，不写状态。
    fn measured_line(&self) -> String {
        if self.cfg.play.measured_max_fps > 0 {
            t!("play.measured", v = self.cfg.play.measured_max_fps).into_owned()
        } else {
            t!("play.unmeasured").into_owned()
        }
    }

    /// half 实测行：有值报预算，无值提示下次启动现测取半。
    fn half_line(&self) -> String {
        if self.cfg.render.measured_vsync_fps > 0 {
            t!("vsync.half_measured", v = self.cfg.render.measured_vsync_fps).into_owned()
        } else {
            t!("vsync.half_unmeasured").into_owned()
        }
    }

    /// 渲染矩阵记忆行：全未测提示切 auto，否则报记忆值 + 校准时机型核数。
    fn render_measured_line(&self) -> String {
        let m = &self.cfg.render;
        if m.measured_render == RenderMode::Auto
            && m.measured_workers == WorkersMode::Auto
            && m.measured_dist == DistMode::Auto
            && m.measured_vsync == VsyncMode::Auto
            && m.measured_cores == 0
        {
            t!("render.unmeasured").into_owned()
        } else {
            t!(
                "render.measured",
                v = format!(
                    "{}/{}/{}/{}, {} cores",
                    m.measured_render.file_str(),
                    m.measured_workers.file_str(),
                    m.measured_dist.file_str(),
                    m.measured_vsync.file_str(),
                    m.measured_cores
                )
            )
            .into_owned()
        }
    }

    fn load_path(&mut self, path: String) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(cfg) => {
                    let cfg = coerce_loaded(cfg);
                    let msg = t!(
                        "status.loaded",
                        path = path.clone(),
                        accel = cfg.accel.level.file_str(),
                        render = cfg.render.mode.file_str()
                    )
                    .into_owned();
                    self.cfg = cfg;
                    self.sync_widgets();
                    self.tune_at_load = self.cfg.render.tune;
                    self.status = msg;
                    self.path = path;
                }
                Err(e) => {
                    self.status =
                        t!("status.parse_error", path = path, err = e.to_string()).into_owned();
                }
            },
            Err(e) => {
                self.status =
                    t!("status.read_error", path = path, err = e.to_string()).into_owned();
            }
        }
    }

    fn save_to(&mut self, path: &str) {
        clamp_tune(&mut self.cfg.render.tune);
        // tune 变了 = bench 条件变了：旧记忆作废，下次启动重测。
        // 不做这个，改 tune 会被“有记忆快启”吞掉，永远不生效。
        if self.cfg.render.tune != self.tune_at_load {
            self.cfg.render.measured_render = RenderMode::Auto;
            self.cfg.render.measured_workers = WorkersMode::Auto;
            self.cfg.render.measured_dist = DistMode::Auto;
            self.cfg.render.measured_vsync = VsyncMode::Auto;
            self.cfg.render.measured_vsync_fps = 0;
            self.cfg.render.measured_cores = 0;
            self.cfg.play.measured_max_fps = 0;
        }
        self.tune_at_load = self.cfg.render.tune;
        match toml::to_string(&self.cfg) {
            Ok(text) => match std::fs::write(path, text) {
                Ok(()) => {
                    self.status = t!("status.saved", path = path).into_owned();
                }
                Err(e) => {
                    self.status =
                        t!("status.write_error", path = path, err = e.to_string()).into_owned();
                }
            },
            Err(e) => {
                self.status = t!("status.serialize_error", err = e.to_string()).into_owned();
            }
        }
    }

    /// 选中态强调色（深浅主题各一，保证在玻璃上可读）。
    fn accent(dark: bool) -> egui::Color32 {
        if dark {
            egui::Color32::LIGHT_BLUE
        } else {
            egui::Color32::from_rgb(0, 90, 200)
        }
    }

    /// 单选开关的选中标记（● ○ 全量字体自带，比例字体下也有字形）。
    fn marker(selected: bool) -> &'static str {
        if selected {
            "●"
        } else {
            "○"
        }
    }

    /// 工具栏/状态条的玻璃材质：深色配深色预设，浅色配默认。
    fn bar_style(&self) -> egui_glass::GlassStyle {
        if self.dark {
            egui_glass::GlassStyle::dark()
        } else {
            egui_glass::GlassStyle::regular()
        }
    }

    fn do_action(&mut self, ctx: &egui::Context, act: Action) {
        match act {
            Action::None => {}
            Action::Open => {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .pick_file()
                {
                    self.load_path(f.display().to_string());
                }
            }
            Action::Save => {
                let p = self.path.clone();
                self.save_to(&p);
            }
            Action::SaveAs => {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .set_file_name("player.toml")
                    .save_file()
                {
                    let p = f.display().to_string();
                    self.path = p.clone();
                    self.save_to(&p);
                }
            }
            Action::Reset => {
                self.cfg = FileConfig::default();
                self.sync_widgets();
                self.tune_at_load = self.cfg.render.tune;
                self.status = t!("status.reset").into_owned();
            }
            Action::ToggleTheme => {
                self.dark = !self.dark;
                ctx.set_visuals(if self.dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                if let Some(rs) = self.rs.as_ref() {
                    egui_glass::set_backdrop(ctx, rs, &make_backdrop(self.dark));
                }
                self.status = t!(
                    "status.theme",
                    theme = if self.dark {
                        t!("theme.dark")
                    } else {
                        t!("theme.light")
                    },
                    bg = if self.dark {
                        t!("theme.bg_black")
                    } else {
                        t!("theme.bg_white")
                    }
                )
                .into_owned();
            }
            Action::ToggleLang => {
                self.zh = !self.zh;
                rust_i18n::set_locale(if self.zh { "zh-CN" } else { "en" });
                // 标题栏跟语言走：英文 UEFI Player / 中文 UEFI播放器。
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
                self.status = t!("status.ready").into_owned();
            }
        }
    }

    /// 顶栏动作按钮：全玻璃（嵌在 toolbar 里，见文件头注释的用户拍板说明）。
    fn action_buttons(&mut self, ui: &mut egui::Ui) -> Action {
        let mut act = Action::None;
        if egui_glass::GlassButton::new(t!("toolbar.open"))
            .show(ui)
            .clicked()
        {
            act = Action::Open;
        }
        if egui_glass::GlassButton::new(t!("toolbar.save"))
            .show(ui)
            .clicked()
        {
            act = Action::Save;
        }
        if egui_glass::GlassButton::new(t!("toolbar.save_as"))
            .show(ui)
            .clicked()
        {
            act = Action::SaveAs;
        }
        if egui_glass::GlassButton::new(t!("toolbar.reset"))
            .show(ui)
            .clicked()
        {
            act = Action::Reset;
        }
        ui.add_space(6.0);
        if egui_glass::GlassButton::new(if self.dark {
            t!("toolbar.to_light")
        } else {
            t!("toolbar.to_dark")
        })
        .show(ui)
        .clicked()
        {
            act = Action::ToggleTheme;
        }
        if egui_glass::GlassButton::new(if self.zh { "EN" } else { "中文" })
            .show(ui)
            .clicked()
        {
            act = Action::ToggleLang;
        }
        act
    }

    /// 左侧导航：全宽玻璃按钮，选中态描蓝 + ●。
    fn nav_button(&mut self, ui: &mut egui::Ui, page: Page, label: String) {
        let selected = self.page == page;
        let mut b = egui_glass::GlassButton::new(format!("{} {}", Self::marker(selected), label))
            .min_size(egui::vec2(ui.available_width(), 30.0));
        if selected {
            b = b.text_color(Self::accent(self.dark));
        }
        if b.show(ui).clicked() {
            self.page = page;
        }
    }

    /// 单选开关：玻璃按钮 + ●○ 标记，点中返回 true（调用方负责赋值）。
    fn option_button(&self, ui: &mut egui::Ui, selected: bool, name: &str) -> bool {
        let mut b = egui_glass::GlassButton::new(format!("{} {}", Self::marker(selected), name));
        if selected {
            b = b.text_color(Self::accent(self.dark));
        }
        b.show(ui).clicked()
    }

    /// 开/关行（两颗玻璃按钮）：返回用户点选后的值，调用方写回。
    /// 纯为了借用简单（直接在循环里拿 `&mut` 字段会和 `option_button` 的 `&self` 打架）。
    fn switch_row(&mut self, ui: &mut egui::Ui, cur: bool, on: &str, off: &str) -> bool {
        let mut next = cur;
        ui.horizontal(|ui| {
            if self.option_button(ui, cur, on) {
                next = true;
            }
            if self.option_button(ui, !cur, off) {
                next = false;
            }
        });
        next
    }

    /// 实时背景的页面层：只留一句提示做折射燃料。`live.run` 会让它跑两遍
    /// （可见 + 离屏），所以这里禁止任何写状态、禁止放玻璃件、
    /// 禁止放可交互控件——只许纯文本。
    /// 摘要行只放左侧导航卡，不在这里重复占右侧空间。
    fn page_backdrop(&self, page: &mut egui::Ui) {
        // 先铺底色：窗口清屏色不可控（实测透黑），两遍（可见 + 离屏）用同一底，
        // 映射才一致。深浅跟主题走。
        let bg = if self.dark {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        };
        page.painter().rect_filled(page.max_rect(), 0.0, bg);
        page.add_space(4.0);
        page.label(egui::RichText::new(t!("sum.hint")).weak().small());
        page.add_space(4.0);
    }

    /// 中央不透明面板里的开关（只拨开关，不暴露 TOML 原文）。
    fn page_content(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::File => {
                ui.heading(t!("file.heading"));
                ui.small(t!("file.hint"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(t!("file.current"));
                    ui.monospace(self.path.as_str());
                });
                ui.add_space(4.0);
                ui.small(t!("file.tip"));
            }
            Page::Accel => {
                ui.heading(t!("accel.heading"));
                ui.add_space(4.0);
                ui.label(t!("accel.level"));
                // 档位滑杆：0-8 拨 `ALL` 下标，文本行同步显示当前档 + 说明。
                let cur = self.cfg.accel.level;
                let accel_label = format!("{}: {}", cur.file_str(), level_desc(cur));
                egui_glass::GlassSlider::new(&mut self.accel_f, 0.0..=8.0)
                    .text(accel_label)
                    .step_by(1.0)
                    .style(if self.dark {
                        egui_glass::GlassStyle::dark()
                    } else {
                        egui_glass::GlassStyle::regular()
                    })
                    .show(ui);
                self.cfg.accel.level =
                    AccelLevel::ALL[self.accel_f.round().clamp(0.0, 8.0) as usize];
                ui.small(t!("accel.auto_level"));
                ui.add_space(10.0);
                ui.label(t!("accel.mode"));
                ui.horizontal(|ui| {
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Auto,
                        &t!("accel.mode_name_auto"),
                    ) {
                        self.cfg.render.mode = RenderMode::Auto;
                    }
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Direct,
                        &t!("accel.mode_name_direct"),
                    ) {
                        self.cfg.render.mode = RenderMode::Direct;
                    }
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Blt,
                        &t!("accel.mode_name_blt"),
                    ) {
                        self.cfg.render.mode = RenderMode::Blt;
                    }
                });
                ui.small(mode_desc(self.cfg.render.mode));
                ui.add_space(10.0);
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.label(t!("accel.blt_op"));
                    // 一行四个（窄窗自动换行保底，不断尾）。
                    // 只有 BufferToVideo 能上屏播视频，其余三项置灰展示、不删：
                    // 库内件无禁用态（`GlassButton::show` 写死 `Sense::click`，
                    // 自绘不跟随禁用样式），手动灰字 + `add_enabled_ui(false)` 关交互。
                    ui.horizontal_wrapped(|ui| {
                        for v in BltOpKind::ALL {
                            if v == BltOpKind::BufferToVideo {
                                if self.option_button(ui, self.cfg.render.blt_op == v, v.file_str())
                                {
                                    self.cfg.render.blt_op = v;
                                }
                            } else {
                                ui.add_enabled_ui(false, |ui| {
                                    egui_glass::GlassButton::new(format!(
                                        "{} {}",
                                        Self::marker(false),
                                        v.file_str()
                                    ))
                                    .text_color(egui::Color32::GRAY)
                                    .show(ui);
                                });
                            }
                        }
                    });
                    ui.small(blt_desc(self.cfg.render.blt_op));
                    ui.add_space(10.0);
                }
                // Blt 恒单线程：workers 整组不可选，直接隐藏（不摆开关也不留提示）。
                if self.cfg.render.mode != RenderMode::Blt {
                    ui.label(t!("accel.workers"));
                    ui.horizontal(|ui| {
                        for v in WorkersMode::ALL {
                            if self.option_button(ui, self.cfg.render.workers == v, v.file_str()) {
                                self.cfg.render.workers = v;
                            }
                        }
                    });
                    ui.small(workers_desc(self.cfg.render.workers));
                    ui.add_space(10.0);
                    // dist 只在直写 + 全核时有意义：单核下整组隐藏。
                    if self.cfg.render.workers != WorkersMode::BspOnly {
                        ui.label(t!("accel.dist"));
                        // 一行四个（窄窗自动换行保底，不断尾；与 blt 行同式）。
                        ui.horizontal_wrapped(|ui| {
                            for v in DistMode::ALL {
                                if self.option_button(ui, self.cfg.render.dist == v, v.file_str()) {
                                    self.cfg.render.dist = v;
                                }
                            }
                        });
                        ui.small(dist_desc(self.cfg.render.dist));
                        ui.add_space(10.0);
                    }
                }
                ui.label(t!("accel.vsync"));
                // 一行四个（窄窗自动换行保底，不断尾；与 dist 行同式）。
                ui.horizontal_wrapped(|ui| {
                    for v in VsyncMode::ALL {
                        if self.option_button(ui, self.cfg.render.vsync == v, v.file_str()) {
                            self.cfg.render.vsync = v;
                        }
                    }
                });
                ui.small(vsync_desc(self.cfg.render.vsync));
                // half 实测行：有值报预算，无值提示下次启动现测取半 + 重测按钮。
                if self.cfg.render.vsync == VsyncMode::Half {
                    ui.horizontal(|ui| {
                        ui.label(self.half_line());
                        if egui_glass::GlassButton::new(t!("vsync.half_rebench"))
                            .show(ui)
                            .clicked()
                        {
                            // 清零并存盘：下次启动 half 会重测。
                            self.cfg.render.measured_vsync_fps = 0;
                            let p = self.path.clone();
                            self.save_to(&p);
                        }
                    });
                }
                ui.add_space(10.0);
                // 微调只摆本模式用得上的，整组无关直接隐藏（判定见 `tune_visible`）。
                let vis = tune_visible(&self.cfg.render);
                if vis.frames || vis.tile || vis.vfps || vis.buf {
                    ui.label(t!("tune.heading"));
                    ui.small(t!("tune.hint"));
                    if vis.frames {
                        ui.label(t!("tune.frames"));
                        let frames_label = format!("{} frames", self.cfg.render.tune.frames);
                        egui_glass::GlassSlider::new(&mut self.tune_frames_f, 1.0..=120.0)
                            .text(frames_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.frames =
                            self.tune_frames_f.round().clamp(1.0, 120.0) as u32;
                    }
                    if vis.tile {
                        ui.label(t!("tune.tile_rows"));
                        let tile_label = format!("{} rows", self.cfg.render.tune.tile_rows);
                        egui_glass::GlassSlider::new(&mut self.tune_tile_f, 8.0..=512.0)
                            .text(tile_label)
                            .step_by(8.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.tile_rows =
                            self.tune_tile_f.round().clamp(8.0, 512.0) as u32;
                    }
                    if vis.vfps {
                        ui.label(t!("tune.vsync_fps"));
                        let vsync_fps_label = format!("{} fps", self.cfg.render.tune.vsync_fps);
                        egui_glass::GlassSlider::new(&mut self.tune_vsync_fps_f, 15.0..=240.0)
                            .text(vsync_fps_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.vsync_fps =
                            self.tune_vsync_fps_f.round().clamp(15.0, 240.0) as u32;
                    }
                    if vis.buf {
                        ui.label(t!("tune.buffers"));
                        let buffers_label = if self.cfg.render.tune.buffers == 0 {
                            t!("tune.buffers_off").into_owned()
                        } else {
                            format!("{} buffers", self.cfg.render.tune.buffers)
                        };
                        egui_glass::GlassSlider::new(&mut self.tune_buffers_f, 0.0..=16.0)
                            .text(buffers_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.buffers =
                            self.tune_buffers_f.round().clamp(0.0, 16.0) as u32;
                    }
                    ui.small(t!("tune.note"));
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    ui.label(self.render_measured_line());
                    if egui_glass::GlassButton::new(t!("render.rebench"))
                        .show(ui)
                        .clicked()
                    {
                        // 清零记忆并存盘：任一 auto 下次启动重校准。
                        // half 预算派生自矩阵计时，一并清零。
                        self.cfg.render.measured_render = RenderMode::Auto;
                        self.cfg.render.measured_workers = WorkersMode::Auto;
                        self.cfg.render.measured_dist = DistMode::Auto;
                        self.cfg.render.measured_vsync = VsyncMode::Auto;
                        self.cfg.render.measured_vsync_fps = 0;
                        self.cfg.render.measured_cores = 0;
                        let p = self.path.clone();
                        self.save_to(&p);
                    }
                });
                ui.add_space(10.0);
                ui.label(t!("play.fps"));
                // 三段滑杆：0 = auto，1-240 = 限速，241 = max。
                // 文本先算好：滑杆拿 &mut play_f 时不能再借 self。
                let play_label = self.play_text();
                egui_glass::GlassSlider::new(&mut self.play_f, 0.0..=241.0)
                    .text(play_label)
                    .step_by(1.0)
                    .style(if self.dark {
                        egui_glass::GlassStyle::dark()
                    } else {
                        egui_glass::GlassStyle::regular()
                    })
                    .show(ui);
                let pos = self.play_f.round().clamp(0.0, 241.0) as u32;
                self.cfg.play.fps = match pos {
                    0 => String::from("auto"),
                    241 => String::from("max"),
                    n => n.to_string(),
                };
                ui.small(t!("play.hint"));
                ui.add_space(4.0);
                ui.label(t!("play.format"));
                self.cfg.play.format = self.format_row(ui, self.cfg.play.format);
                ui.small(t!("play.format_hint"));
                ui.horizontal(|ui| {
                    ui.label(self.measured_line());
                    if egui_glass::GlassButton::new(t!("play.rebench"))
                        .show(ui)
                        .clicked()
                    {
                        // 清零并存盘：下次启动 auto 会重测。
                        self.cfg.play.measured_max_fps = 0;
                        let p = self.path.clone();
                        self.save_to(&p);
                    }
                });
            }
            Page::Wallpaper => {
                ui.heading(t!("wallpaper.heading"));
                ui.small(t!("wallpaper.hint"));
                ui.add_space(4.0);
                ui.label(t!("wallpaper.enable"));
                ui.horizontal(|ui| {
                    if self.option_button(ui, self.cfg.wallpaper.enable, &t!("wallpaper.on")) {
                        self.cfg.wallpaper.enable = true;
                    }
                    if self.option_button(ui, !self.cfg.wallpaper.enable, &t!("wallpaper.off")) {
                        self.cfg.wallpaper.enable = false;
                    }
                });
                ui.small(wp_enable_desc(self.cfg.wallpaper.enable));
                // 关壁纸时格式/帧率/帧数/预载全不可选，整组隐藏。
                if self.cfg.wallpaper.enable {
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.format"));
                    self.cfg.wallpaper.format = self.format_row(ui, self.cfg.wallpaper.format);
                    ui.small(t!("wallpaper.format_hint"));
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.popup_freeze"));
                    let froze = self.switch_row(ui, self.cfg.wallpaper.popup_freeze, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.wallpaper.popup_freeze = froze;
                    ui.small(t!("wallpaper.popup_freeze_hint"));
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.fps"));
                    let fps_label = format!("{} fps", self.cfg.wallpaper.fps);
                    egui_glass::GlassSlider::new(&mut self.wp_fps_f, 1.0..=30.0)
                        .text(fps_label)
                        .step_by(1.0)
                        .style(if self.dark {
                            egui_glass::GlassStyle::dark()
                        } else {
                            egui_glass::GlassStyle::regular()
                        })
                        .show(ui);
                    self.cfg.wallpaper.fps = self.wp_fps_f.round().clamp(1.0, 30.0) as u32;
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.frames"));
                let frames_label = if self.cfg.wallpaper.frames == 0 {
                    t!("wallpaper.frames_all").into_owned()
                } else {
                    format!("{} frames", self.cfg.wallpaper.frames)
                };
                egui_glass::GlassSlider::new(&mut self.wp_frames_f, 0.0..=120.0)
                    .text(frames_label)
                    .step_by(1.0)
                    .style(if self.dark {
                        egui_glass::GlassStyle::dark()
                    } else {
                        egui_glass::GlassStyle::regular()
                    })
                    .show(ui);
                self.cfg.wallpaper.frames = self.wp_frames_f.round().clamp(0.0, 120.0) as u32;
                ui.small(t!("wallpaper.frames_hint"));
                ui.add_space(10.0);
                ui.label(t!("wallpaper.preload"));
                let preload_label = format!("{} frames", self.cfg.wallpaper.preload);
                egui_glass::GlassSlider::new(&mut self.wp_preload_f, 1.0..=16.0)
                    .text(preload_label)
                    .step_by(1.0)
                    .style(if self.dark {
                        egui_glass::GlassStyle::dark()
                    } else {
                        egui_glass::GlassStyle::regular()
                    })
                    .show(ui);
                self.cfg.wallpaper.preload = self.wp_preload_f.round().clamp(1.0, 16.0) as u32;
                ui.small(t!("wallpaper.preload_hint"));
                }
            }
            Page::Boot => {
                ui.heading(t!("boot.heading"));
                ui.small(t!("boot.hint"));
                ui.add_space(4.0);
                ui.label(t!("boot.auto"));
                let auto = self.switch_row(ui, self.cfg.boot.auto, &t!("boot.on"), &t!("boot.off"));
                self.cfg.boot.auto = auto;
                ui.small(t!("boot.auto_desc"));
                // 总开关关时细项不可选，整组隐藏（壁纸页同式）。
                if self.cfg.boot.auto {
                    ui.add_space(10.0);
                    ui.label(t!("boot.windows"));
                    let v = self.switch_row(ui, self.cfg.boot.windows, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.windows = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.linux"));
                    let v = self.switch_row(ui, self.cfg.boot.linux, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.linux = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.shell"));
                    let v = self.switch_row(ui, self.cfg.boot.shell, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.shell = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.uki"));
                    let v = self.switch_row(ui, self.cfg.boot.uki, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.uki = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.macos"));
                    let v = self.switch_row(ui, self.cfg.boot.macos, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.macos = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.strict"));
                    let v = self.switch_row(ui, self.cfg.boot.strict, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.strict = v;
                }
            }
            Page::Summary => {
                ui.heading(t!("sum.heading"));
                ui.small(t!("sum.hint"));
                ui.add_space(8.0);
                ui.label(t!("sum.accel", v = self.cfg.accel.level.file_str()));
                ui.label(t!("sum.render", v = self.cfg.render.mode.file_str()));
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.label(t!("sum.blt", v = self.cfg.render.blt_op.file_str()));
                }
                ui.label(t!("sum.workers", v = self.cfg.render.workers.file_str()));
                ui.label(t!("sum.dist", v = self.cfg.render.dist.file_str()));
                ui.label(t!("sum.vsync", v = self.cfg.render.vsync.file_str()));
                ui.label(self.render_measured_line());
                ui.label(t!(
                    "sum.wallpaper",
                    v = if self.cfg.wallpaper.enable {
                        format!(
                            "on {}/{}fps/{}f/pre{}/fz{}",
                            self.cfg.wallpaper.format.file_str(),
                            self.cfg.wallpaper.fps,
                            self.cfg.wallpaper.frames,
                            self.cfg.wallpaper.preload,
                            self.cfg.wallpaper.popup_freeze as u8,
                        )
                    } else {
                        String::from("off")
                    }
                ));
                ui.label(t!("sum.play", v = self.play_text()));
                ui.label(self.measured_line());
                ui.label(t!(
                    "sum.boot",
                    v = if self.cfg.boot.auto {
                        format!(
                            "auto win:{} lin:{} sh:{} uki:{} mac:{} st:{}",
                            self.cfg.boot.windows as u8,
                            self.cfg.boot.linux as u8,
                            self.cfg.boot.shell as u8,
                            self.cfg.boot.uki as u8,
                            self.cfg.boot.macos as u8,
                            self.cfg.boot.strict as u8,
                        )
                    } else {
                        String::from("off")
                    }
                ));
                ui.label(t!("sum.version", v = self.cfg.version));
                ui.label(t!("sum.file", v = self.path.clone()));
                ui.add_space(8.0);
                if egui_glass::GlassButton::new(t!("toolbar.save"))
                    .show(ui)
                    .clicked()
                {
                    let p = self.path.clone();
                    self.save_to(&p);
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 无 GPU 时的兜底：单面板堆叠。玻璃管线没注册，玻璃件不可用，
        // 这里只能用普通控件（该分支实际走不到，renderer 已强制 Wgpu）。
        if !self.glass {
            let mut act = Action::None;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.page, Page::File, t!("nav.file"));
                    ui.selectable_value(&mut self.page, Page::Accel, t!("nav.accel"));
                    ui.selectable_value(&mut self.page, Page::Wallpaper, t!("nav.wallpaper"));
                    ui.selectable_value(&mut self.page, Page::Boot, t!("nav.boot"));
                    ui.selectable_value(&mut self.page, Page::Summary, t!("nav.summary"));
                    if ui.button(t!("toolbar.open")).clicked() {
                        act = Action::Open;
                    }
                    if ui.button(t!("toolbar.save")).clicked() {
                        act = Action::Save;
                    }
                    if ui.button(if self.zh { "EN" } else { "中文" }).clicked() {
                        act = Action::ToggleLang;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                ui.separator();
                ui.label(&self.status);
            });
            self.do_action(ui.ctx(), act);
            return;
        }

        // （实时背景在中央面板内容头，见下。）

        // 顶栏：文件动作工具条胶囊（误伤恢复：真凶是 Panel 分隔线，现已全关，
        // 胶囊本身圆角无直边）。
        let mut act = Action::None;
        egui::Panel::top("toolbar")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    egui_glass::GlassToolbar::new(self.bar_style()).show(ui, |ui| {
                        // 顶栏不放标题（窗口标题栏 + 导航/文件页已有，不重复）。
                        act = self.action_buttons(ui);
                    });
                });
                ui.add_space(8.0);
            });

        // 底栏：状态胶囊（同上，误伤恢复）。
        egui::Panel::bottom("status")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    egui_glass::GlassToolbar::new(self.bar_style()).show(ui, |ui| {
                        ui.label(&self.status);
                    });
                });
                ui.add_space(8.0);
            });

        // 左侧：玻璃导航卡。
        egui::Panel::left("nav")
            .default_size(216.0)
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                let card = if self.dark {
                    egui_glass::Glass::panel_dark()
                } else {
                    egui_glass::Glass::panel()
                };
                card.inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.heading(t!("nav.sections"));
                    self.nav_button(ui, Page::File, t!("nav.file").into_owned());
                    self.nav_button(ui, Page::Accel, t!("nav.accel").into_owned());
                    self.nav_button(ui, Page::Wallpaper, t!("nav.wallpaper").into_owned());
                    self.nav_button(ui, Page::Boot, t!("nav.boot").into_owned());
                    self.nav_button(ui, Page::Summary, t!("nav.summary").into_owned());
                    ui.add_space(8.0);
                    ui.small(t!("sum.accel", v = self.cfg.accel.level.file_str()));
                    ui.small(t!("sum.render", v = self.cfg.render.mode.file_str()));
                    ui.small(t!("sum.workers", v = self.cfg.render.workers.file_str()));
                    ui.small(t!("sum.dist", v = self.cfg.render.dist.file_str()));
                    ui.small(t!("sum.vsync", v = self.cfg.render.vsync.file_str()));
                    ui.small(t!(
                        "sum.wallpaper",
                        v = if self.cfg.wallpaper.enable {
                            "on"
                        } else {
                            "off"
                        }
                    ));
                    ui.small(t!("sum.play", v = self.play_text()));
                    ui.small(self.measured_line());
                    ui.small(t!("sum.version", v = self.cfg.version));
                });
                ui.add_space(8.0);
            });

        // 中央：大号玻璃薄片（误伤恢复：之前误判薄片直边是分割线，
        // 实为 Panel 分隔线，现已全关）。稠密表单放 panel frost 上。
        // 中央最后加（Panel 之后）。
        //
        // 实时背景只能接在这里：`live.run` 要求调用时光标全新（twin 用全新
        // Ui 重排一次，两遍光标必须一致）——根 ui 上画页面会把 Panel 全挤下去，
        // 独立 Area 实测也会打乱 Panel 布局（原因未明，宁可信截屏），
        // 只有中央面板内容头是安全的：兄弟 Panel 已先占位，内部只是多滚一段。
        // 页面是弱化摘要纸（折射燃料），表单跟在后面正常滚。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let sheet = if self.dark {
                    egui_glass::Glass::panel_dark()
                } else {
                    egui_glass::Glass::panel()
                };
                sheet.inner_margin(egui::Margin::same(16)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    if let Some(live) = self.live.as_ref() {
                        let clear = if self.dark {
                            egui::Color32::BLACK
                        } else {
                            egui::Color32::WHITE
                        };
                        live.run(ui, clear, |page| self.page_backdrop(page));
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                });
            });

        self.do_action(ui.ctx(), act);
    }
}

/// 文件 schema 回归：旧文件（无 `[wallpaper]` 表）必须照读，新字段回写不丢。
#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn old_file_without_wallpaper_parses() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(!cfg.wallpaper.enable);
        assert_eq!(cfg.wallpaper.fps, default_wallpaper_fps());
        assert_eq!(cfg.wallpaper.preload, default_wallpaper_preload());
        assert_eq!(cfg.render.dist, DistMode::Auto);
        assert_eq!(cfg.render.measured_dist, DistMode::Auto);
    }

    #[test]
    fn tune_fields_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.render.tune.frames = 45;
        cfg.render.tune.tile_rows = 32;
        cfg.render.tune.vsync_fps = 90;
        cfg.render.tune.buffers = 5;
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.render.tune.frames, 45);
        assert_eq!(back.render.tune.tile_rows, 32);
        assert_eq!(back.render.tune.vsync_fps, 90);
        assert_eq!(back.render.tune.buffers, 5);
    }

    #[test]
    fn tune_missing_keys_use_defaults() {
        // 比 wallpaper 测试更老的文件：连 [render.tune] 都没有。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.tune.frames, default_frames());
        assert_eq!(cfg.render.tune.tile_rows, default_tile_rows());
        assert_eq!(cfg.render.tune.vsync_fps, default_vsync_fps());
        assert_eq!(cfg.render.tune.buffers, default_buffers());
    }

    #[test]
    fn vsync_half_roundtrip_and_on_alias() {
        // 新档位：half 照读照写，预算回写不丢。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"direct\"\nworkers = \"all\"\nvsync = \"half\"\n[play]\nfps = \"auto\"\n";
        let mut cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.vsync, VsyncMode::Half);
        cfg.render.measured_vsync_fps = 42;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.render.vsync, VsyncMode::Half);
        assert_eq!(back.render.measured_vsync_fps, 42);
        // 旧文件 `"on"` 按 static 照读（固件同 alias，不断旧配置）。
        let old: FileConfig =
            toml::from_str("[render]\nvsync = \"on\"\n").unwrap();
        assert_eq!(old.render.vsync, VsyncMode::Static);
    }

    #[test]
    fn dist_dynamic_roundtrip() {
        // 新档位：显式 dynamic 照读照写，measured 同理（旧文件无该值不受影响）。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"direct\"\nworkers = \"all\"\ndist = \"dynamic\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.dist, DistMode::Dynamic);
        let mut cfg = cfg;
        cfg.render.measured_dist = DistMode::Dynamic;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.render.dist, DistMode::Dynamic);
        assert_eq!(back.render.measured_dist, DistMode::Dynamic);
    }

    #[test]
    fn wallpaper_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.wallpaper.enable = true;
        cfg.wallpaper.fps = 20;
        cfg.wallpaper.frames = 30;
        cfg.wallpaper.format = MediaFormat::Qois;
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert!(back.wallpaper.enable);
        assert_eq!(back.wallpaper.fps, 20);
        assert_eq!(back.wallpaper.frames, 30);
        assert_eq!(back.wallpaper.format, MediaFormat::Qois);
    }
}

/// 配置黑盒矩阵：GUI 改配置 → 落盘 → 固件生效，全链路只走 TOML 文本 + 纯规则，
/// 无 display 下 `cargo test` 可跑。新增 GUI 规则/档位/字段时先在这里加行
/// （表格即口径），再接 UI 与固件（见 AGENTS「黑盒矩阵测试」；QEMU 侧见
/// `tools/qemu-matrix/matrix.toml`，两边一起更新）。
#[cfg(test)]
mod blackbox_matrix {
    use super::*;

    fn rc(
        mode: RenderMode,
        workers: WorkersMode,
        dist: DistMode,
        vsync: VsyncMode,
    ) -> RenderConfig {
        let mut m = RenderConfig::default();
        m.mode = mode;
        m.workers = workers;
        m.dist = dist;
        m.vsync = vsync;
        m
    }

    /// 全档位笛卡尔回写：9 accel × 3 render × 3 workers × 4 dist × 4 vsync × 4 blt，
    /// 序列化再解析必须逐字节等价（顺序/缺键都由 serde 保证，不许丢字段）。
    #[test]
    fn schema_roundtrip_matrix() {
        let mut n = 0;
        for a in AccelLevel::ALL {
            for m in [RenderMode::Auto, RenderMode::Direct, RenderMode::Blt] {
                for w in WorkersMode::ALL {
                    for d in DistMode::ALL {
                        for v in VsyncMode::ALL {
                            for b in BltOpKind::ALL {
                                let cfg = FileConfig {
                                    version: 1,
                                    accel: AccelConfig { level: a },
                                    render: RenderConfig {
                                        mode: m,
                                        workers: w,
                                        dist: d,
                                        vsync: v,
                                        blt_op: b,
                                        tune: RenderTune {
                                            frames: 7,
                                            tile_rows: 100,
                                            vsync_fps: 77,
                                            buffers: 3,
                                        },
                                        measured_render: m,
                                        measured_workers: w,
                                        measured_dist: d,
                                        measured_vsync: v,
                                        measured_vsync_fps: 11,
                                        measured_cores: 4,
                                    },
                                    play: PlayConfig {
                                        fps: String::from("60"),
                                        measured_max_fps: 120,
                                        format: MediaFormat::Raw,
                                    },
                                wallpaper: WallpaperConfig {
                                    enable: true,
                                    fps: 20,
                                    frames: 30,
                                    preload: 4,
                                    format: MediaFormat::Qois,
                                    popup_freeze: true,
                                },
                    boot: BootConfig {
                        auto: true,
                        windows: true,
                        linux: false,
                        shell: true,
                        uki: false,
                        macos: true,
                        strict: false,
                    },
                            };
                                let text = toml::to_string(&cfg).unwrap();
                                let back: FileConfig = toml::from_str(&text).unwrap();
                                assert_eq!(back, cfg, "roundtrip {a:?}/{m:?}/{w:?}/{d:?}/{v:?}/{b:?}");
                                n += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 9 * 3 * 3 * 4 * 4 * 4);
    }

    /// fps 三态字符串照读照写（非法值不断解析，语义由 `parse_fps` 兜底）。
    #[test]
    fn fps_string_matrix() {
        for s in ["auto", "max", "0", "1", "60", "120", "240", "999", "abc", ""] {
            let text = format!("[play]\nfps = \"{s}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.play.fps, s);
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.play.fps, s);
        }
    }

    /// 远古文件（v0.1 时代：无 wallpaper/tune 表）照读，新键全按默认。
    #[test]
    fn compat_legacy_matrix() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(!cfg.wallpaper.enable);
        assert_eq!(cfg.wallpaper.format, MediaFormat::Raw);
        assert!(cfg.wallpaper.popup_freeze);
        assert_eq!(cfg.play.format, MediaFormat::Qois);
        assert_eq!(cfg.render.dist, DistMode::Auto);
        assert_eq!(cfg.render.vsync, VsyncMode::Auto);
        assert_eq!(cfg.render.blt_op, BltOpKind::BufferToVideo);
        assert_eq!(cfg.render.tune.buffers, default_buffers());
        assert_eq!(cfg.render.measured_vsync_fps, 0);
    }

    /// 片源格式双档位回写（play/wallpaper 各 qois/raw 四组合，不丢字段）。
    /// 弹窗冻结开关同步翻转（8 组合，见 `[wallpaper] popup_freeze`）。
    #[test]
    fn format_roundtrip_matrix() {
        for (pf, wf) in [
            (MediaFormat::Qois, MediaFormat::Qois),
            (MediaFormat::Qois, MediaFormat::Raw),
            (MediaFormat::Raw, MediaFormat::Qois),
            (MediaFormat::Raw, MediaFormat::Raw),
        ] {
            for froze in [false, true] {
                let mut cfg = FileConfig::default();
                cfg.play.format = pf;
                cfg.wallpaper.format = wf;
                cfg.wallpaper.popup_freeze = froze;
                let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
                assert_eq!(back.play.format, pf);
                assert_eq!(back.wallpaper.format, wf);
                assert_eq!(back.wallpaper.popup_freeze, froze);
                assert_eq!(back, cfg);
                // 老文件缺键：play 按 qois、wallpaper 按 raw/冻结照读。
                let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
                assert_eq!(legacy.play.format, MediaFormat::Qois);
                assert_eq!(legacy.wallpaper.format, MediaFormat::Raw);
                assert!(legacy.wallpaper.popup_freeze);
            }
        }
    }

    /// 空文件全默认（缺表缺键一路 Default，不 halt）。
    #[test]
    fn compat_empty_matrix() {
        let cfg: FileConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, FileConfig::default());
    }

    /// `[boot]` 缺表：老文件照读，自动发现全开（默认行为就是发现）。
    #[test]
    fn boot_compat_matrix() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(cfg.boot.auto);
        assert!(cfg.boot.windows);
        assert!(cfg.boot.linux);
        assert!(cfg.boot.shell);
        assert!(cfg.boot.uki);
        assert!(cfg.boot.macos);
        assert!(!cfg.boot.strict);
    }

    /// `[boot]` 6 开关全笛卡尔回写（128 种）。
    #[test]
    fn boot_roundtrip_matrix() {
        let mut n = 0;
        for bits in 0..128u8 {
            let mut cfg = FileConfig::default();
            cfg.boot.auto = bits & 1 != 0;
            cfg.boot.windows = bits & 2 != 0;
            cfg.boot.linux = bits & 4 != 0;
            cfg.boot.shell = bits & 8 != 0;
            cfg.boot.uki = bits & 16 != 0;
            cfg.boot.macos = bits & 32 != 0;
            cfg.boot.strict = bits & 64 != 0;
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back, cfg, "{bits:07b}");
            n += 1;
        }
        assert_eq!(n, 128);
    }

    /// 读盘钳制矩阵：只有 B2V 能留，其余三项一律钳回 B2V（播放行为不变）。
    #[test]
    fn coerce_matrix() {
        for (input, expected) in [
            ("buffer_to_video", BltOpKind::BufferToVideo),
            ("video_to_video", BltOpKind::BufferToVideo),
            ("video_fill", BltOpKind::BufferToVideo),
            ("video_to_blt_buffer", BltOpKind::BufferToVideo),
        ] {
            let text = format!("[render]\nblt_op = \"{input}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(coerce_loaded(cfg).render.blt_op, expected, "{input}");
        }
    }

    /// 落盘钳制矩阵：越界钳住，正常值不动。
    #[test]
    fn clamp_matrix() {
        let mut t = RenderTune { frames: 0, tile_rows: 4, vsync_fps: 999, buffers: 99 };
        clamp_tune(&mut t);
        assert_eq!(t, RenderTune { frames: 1, tile_rows: 8, vsync_fps: 240, buffers: 16 });
        let mut ok = RenderTune { frames: 30, tile_rows: 64, vsync_fps: 60, buffers: 2 };
        clamp_tune(&mut ok);
        assert_eq!(ok, RenderTune { frames: 30, tile_rows: 64, vsync_fps: 60, buffers: 2 });
    }

    /// 待测判定矩阵（与固件 `needs_render_bench` 同逻辑 + half）。
    /// 行：(mode, workers, dist, vsync, measured..., cores, half_fps, 期待)。
    #[test]
    fn bench_pending_matrix() {
        // 基线：全 auto 无记忆 → 必测。
        assert!(bench_pending(&RenderConfig::default()));
        // 全显式（Direct/BspOnly/Static/Off）→ 跳过。
        let mut m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Off);
        assert!(!bench_pending(&m));
        // Direct + workers auto 无记忆 → 测 workers。
        m = rc(RenderMode::Direct, WorkersMode::Auto, DistMode::Auto, VsyncMode::Off);
        assert!(bench_pending(&m));
        // workers 已记忆 BspOnly → dist 不参测 → 跳过。
        m.measured_workers = WorkersMode::BspOnly;
        assert!(!bench_pending(&m));
        // Direct + 全核 + dist auto 无记忆 → 测 dist。
        m = rc(RenderMode::Direct, WorkersMode::All, DistMode::Auto, VsyncMode::Off);
        assert!(bench_pending(&m));
        // dist 已记忆 → 跳过。
        m.measured_dist = DistMode::Static;
        assert!(!bench_pending(&m));
        // Blt 定死：workers/dist 不参测（auto 也白搭）。
        m = rc(RenderMode::Blt, WorkersMode::Auto, DistMode::Auto, VsyncMode::Off);
        assert!(!bench_pending(&m));
        // mode auto 无记忆 → 必测（哪怕 Blt 倾向，首轮也得跑）。
        m = rc(RenderMode::Auto, WorkersMode::Auto, DistMode::Auto, VsyncMode::Off);
        assert!(bench_pending(&m));
        // mode auto 已记忆 Direct + workers auto 无记忆 → 测 workers。
        m.measured_render = RenderMode::Direct;
        assert!(bench_pending(&m));
        // mode auto 已记忆 Blt + workers auto 无记忆 → workers 被忽略 → 跳过。
        m.measured_render = RenderMode::Blt;
        assert!(!bench_pending(&m));
        // vsync auto 无记忆 → 测（其余全显式）。
        m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Auto);
        assert!(bench_pending(&m));
        // vsync auto 已记忆 → 跳过。
        m.measured_vsync = VsyncMode::Static;
        assert!(!bench_pending(&m));
        // half 无记忆 → 必测（其余全显式）。
        m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Half);
        assert!(bench_pending(&m));
        // half 已记忆 → 跳过。
        m.measured_vsync_fps = 45;
        assert!(!bench_pending(&m));
        // 显式 half + dist auto 无记忆（全核）→ dist 照测。
        m = rc(RenderMode::Direct, WorkersMode::All, DistMode::Auto, VsyncMode::Half);
        m.measured_vsync_fps = 45;
        assert!(bench_pending(&m));
        // Blt 引擎对照：Blt 在测 + 选中非 B2V → 必测（只出数）。
        m = rc(RenderMode::Blt, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Off);
        m.blt_op = BltOpKind::VideoToBltBuffer;
        assert!(bench_pending(&m));
        // 同上但选中 B2V → B2V 格本就覆盖，无额外待测。
        m.blt_op = BltOpKind::BufferToVideo;
        assert!(!bench_pending(&m));
        // Blt 不在测（Direct 定死）→ 选中啥都不测 op 格。
        m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Off);
        m.blt_op = BltOpKind::VideoFill;
        assert!(!bench_pending(&m));
        // mode auto 已记忆 Blt + 选中非 B2V → Blt 在测 → 测 op 格。
        m = rc(RenderMode::Auto, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Off);
        m.measured_render = RenderMode::Blt;
        m.blt_op = BltOpKind::VideoFill;
        assert!(bench_pending(&m));
    }

    /// 微调显隐矩阵：行 = (mode, workers, dist, vsync) → (frames, tile, vfps, buf)。
    #[test]
    fn tune_visible_matrix() {
        // 全 auto 无记忆：全摆。
        assert_eq!(
            tune_visible(&RenderConfig::default()),
            TuneVisible { frames: true, tile: true, vfps: true, buf: true }
        );
        // Blt 定死：只剩 vfps（auto）+ frames（render auto 无记忆）。
        let m = rc(RenderMode::Blt, WorkersMode::Auto, DistMode::Auto, VsyncMode::Auto);
        assert_eq!(
            tune_visible(&m),
            TuneVisible { frames: true, tile: false, vfps: true, buf: false }
        );
        // 全显式 Direct/BspOnly/Static/Off：只剩 buf。
        let m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Off);
        assert_eq!(
            tune_visible(&m),
            TuneVisible { frames: false, tile: false, vfps: false, buf: true }
        );
        // tile 沾边（auto/Tile）→ tile 摆；static/dynamic 藏。
        for (d, tile) in [
            (DistMode::Auto, true),
            (DistMode::Tile, true),
            (DistMode::Static, false),
            (DistMode::Dynamic, false),
        ] {
            let mut m = rc(RenderMode::Direct, WorkersMode::All, d, VsyncMode::Off);
            // dist 无记忆会点亮 frames，先给记忆压住，只看 tile。
            m.measured_dist = DistMode::Static;
            assert_eq!(
                tune_visible(&m),
                TuneVisible { frames: false, tile, vfps: false, buf: true },
                "{d:?}"
            );
        }
        // vfps 只在 static/auto 摆；half/off 藏（half 自带预算）。
        for (v, vfps) in [
            (VsyncMode::Auto, true),
            (VsyncMode::Static, true),
            (VsyncMode::Half, false),
            (VsyncMode::Off, false),
        ] {
            let mut m = rc(
                RenderMode::Direct,
                WorkersMode::BspOnly,
                DistMode::Static,
                v,
            );
            // 无关记忆先压住（auto 无记忆会点亮 frames，half 无记忆同理），只看 vfps。
            m.measured_vsync = VsyncMode::Static;
            m.measured_vsync_fps = 45;
            let vis = tune_visible(&m);
            assert_eq!(vis.vfps, vfps, "{v:?}");
            assert!(!vis.frames && !vis.tile && vis.buf, "{v:?}");
        }
        // half 无记忆 → frames 亮（自带 bench）。
        let m = rc(RenderMode::Direct, WorkersMode::BspOnly, DistMode::Static, VsyncMode::Half);
        assert!(tune_visible(&m).frames);
    }
}

/// 纯色背景：白底/黑底。均匀底玻璃近乎平（官方原话），深浅色模式优先，不带资源文件。
fn make_backdrop(dark: bool) -> egui::ColorImage {
    let c = if dark {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    };
    egui::ColorImage::filled([256, 256], c)
}

/// 窗口图标：`assets/icon-256.rgba` 预烘焙 256x256 RGBA，直编二进制零新依赖
///（Windows 任务栏/Linux 可见；macOS Dock 图标走 bundle 的 `icon.icns`，
/// 见 `bundle_macos.sh`，运行时设置在 mac 上被忽略）。
fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon-256.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}

fn main() -> eframe::Result<()> {
    rust_i18n::set_locale("en");
    let opts = eframe::NativeOptions {
        // egui_glass 硬性要求 wgpu 渲染器，glow 不可用。
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 680.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "UEFI Player",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
