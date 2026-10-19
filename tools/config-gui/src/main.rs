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

/// 垂直同步（与固件 `VsyncMode` 同构；开 = 叠加 60fps 软件帧预算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VsyncMode {
    #[default]
    Auto,
    On,
    Off,
}

impl VsyncMode {
    const ALL: [Self; 3] = [Self::Auto, Self::On, Self::Off];
    fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RenderTune {
    #[serde(default = "default_frames")]
    frames: u32,
    #[serde(default = "default_tile_rows")]
    tile_rows: u32,
    #[serde(default = "default_vsync_fps")]
    vsync_fps: u32,
}

impl Default for RenderTune {
    fn default() -> Self {
        Self {
            frames: default_frames(),
            tile_rows: default_tile_rows(),
            vsync_fps: default_vsync_fps(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    measured_cores: u32,
}

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），这里只读展示 + 重测清零。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PlayConfig {
    #[serde(default = "default_fps")]
    fps: String,
    #[serde(default)]
    measured_max_fps: u32,
}

/// `[wallpaper]`：与固件 `WallpaperConfig` 同构。无校准记忆，纯开关。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct WallpaperConfig {
    enable: bool,
    #[serde(default = "default_wallpaper_fps")]
    fps: u32,
    #[serde(default)]
    frames: u32,
    #[serde(default = "default_wallpaper_preload")]
    preload: u32,
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

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            measured_max_fps: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct FileConfig {
    version: u32,
    accel: AccelConfig,
    render: RenderConfig,
    play: PlayConfig,
    wallpaper: WallpaperConfig,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            version: 1,
            accel: AccelConfig::default(),
            render: RenderConfig::default(),
            play: PlayConfig::default(),
            wallpaper: WallpaperConfig::default(),
        }
    }
}

// ---------- App ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    File,
    Accel,
    Wallpaper,
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
        VsyncMode::On => t!("accel.desc_vsync_on"),
        VsyncMode::Off => t!("accel.desc_vsync_off"),
    }
    .into_owned()
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
    /// 微调滑杆镜像：frames 1-120，tile_rows 8-512（步进 8），vsync_fps 15-240。
    tune_frames_f: f32,
    tune_tile_f: f32,
    tune_vsync_fps_f: f32,
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
    }

    /// 滑杆/摘要共用的档位文本。
    fn play_text(&self) -> String {
        match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => t!("play.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n),
        }
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
        // 落盘前钳住 tune（滑杆已钳，这里防手改文件后的脏值回写）。
        self.cfg.render.tune.frames = self.cfg.render.tune.frames.max(1);
        self.cfg.render.tune.tile_rows = self.cfg.render.tune.tile_rows.clamp(8, 512);
        self.cfg.render.tune.vsync_fps = self.cfg.render.tune.vsync_fps.clamp(15, 240);
        // tune 变了 = bench 条件变了：旧记忆作废，下次启动重测。
        // 不做这个，改 tune 会被“有记忆快启”吞掉，永远不生效。
        if self.cfg.render.tune != self.tune_at_load {
            self.cfg.render.measured_render = RenderMode::Auto;
            self.cfg.render.measured_workers = WorkersMode::Auto;
            self.cfg.render.measured_dist = DistMode::Auto;
            self.cfg.render.measured_vsync = VsyncMode::Auto;
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
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.label(t!("accel.blt_op"));
                    // 一行四个（窄窗自动换行保底，不断尾）。
                    ui.horizontal_wrapped(|ui| {
                        for v in BltOpKind::ALL {
                            if self.option_button(ui, self.cfg.render.blt_op == v, v.file_str()) {
                                self.cfg.render.blt_op = v;
                            }
                        }
                    });
                    ui.small(blt_desc(self.cfg.render.blt_op));
                } else {
                    ui.small(t!("accel.blt_hidden"));
                }
                ui.add_space(10.0);
                ui.label(t!("accel.workers"));
                // Blt 恒单线程，workers 无意义：只给提示，不摆开关。
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.small(t!("accel.workers_hidden"));
                } else {
                    ui.horizontal(|ui| {
                        for v in WorkersMode::ALL {
                            if self.option_button(ui, self.cfg.render.workers == v, v.file_str()) {
                                self.cfg.render.workers = v;
                            }
                        }
                    });
                    ui.small(workers_desc(self.cfg.render.workers));
                }
                ui.add_space(10.0);
                ui.label(t!("accel.dist"));
                // dist 只在直写 + 全核时有意义：Blt 下无 workers 也就无 dist。
                if self.cfg.render.mode == RenderMode::Blt
                    || self.cfg.render.workers == WorkersMode::BspOnly
                {
                    ui.small(t!("accel.dist_hidden"));
                } else {
                    // 一行四个（窄窗自动换行保底，不断尾；与 blt 行同式）。
                    ui.horizontal_wrapped(|ui| {
                        for v in DistMode::ALL {
                            if self.option_button(ui, self.cfg.render.dist == v, v.file_str()) {
                                self.cfg.render.dist = v;
                            }
                        }
                    });
                    ui.small(dist_desc(self.cfg.render.dist));
                }
                ui.add_space(10.0);
                ui.label(t!("accel.vsync"));
                ui.horizontal(|ui| {
                    for v in VsyncMode::ALL {
                        if self.option_button(ui, self.cfg.render.vsync == v, v.file_str()) {
                            self.cfg.render.vsync = v;
                        }
                    }
                });
                ui.small(vsync_desc(self.cfg.render.vsync));
                ui.add_space(10.0);
                ui.label(t!("tune.heading"));
                ui.small(t!("tune.hint"));
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
                self.cfg.render.tune.frames = self.tune_frames_f.round().clamp(1.0, 120.0) as u32;
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
                self.cfg.render.tune.tile_rows = self.tune_tile_f.round().clamp(8.0, 512.0) as u32;
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
                ui.small(t!("tune.note"));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(self.render_measured_line());
                    if egui_glass::GlassButton::new(t!("render.rebench"))
                        .show(ui)
                        .clicked()
                    {
                        // 清零记忆并存盘：任一 auto 下次启动重校准。
                        self.cfg.render.measured_render = RenderMode::Auto;
                        self.cfg.render.measured_workers = WorkersMode::Auto;
                        self.cfg.render.measured_dist = DistMode::Auto;
                        self.cfg.render.measured_vsync = VsyncMode::Auto;
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
                            "on {}fps/{}f/pre{}",
                            self.cfg.wallpaper.fps,
                            self.cfg.wallpaper.frames,
                            self.cfg.wallpaper.preload
                        )
                    } else {
                        String::from("off")
                    }
                ));
                ui.label(t!("sum.play", v = self.play_text()));
                ui.label(self.measured_line());
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
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.render.tune.frames, 45);
        assert_eq!(back.render.tune.tile_rows, 32);
        assert_eq!(back.render.tune.vsync_fps, 90);
    }

    #[test]
    fn tune_missing_keys_use_defaults() {
        // 比 wallpaper 测试更老的文件：连 [render.tune] 都没有。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.tune.frames, default_frames());
        assert_eq!(cfg.render.tune.tile_rows, default_tile_rows());
        assert_eq!(cfg.render.tune.vsync_fps, default_vsync_fps());
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
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert!(back.wallpaper.enable);
        assert_eq!(back.wallpaper.fps, 20);
        assert_eq!(back.wallpaper.frames, 30);
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
