//! `player.toml` schema：与固件 `src/config.rs` 同构（字段名/枚举字符串一致，改动双边同步）。
//!
//! 纯数据类型 + 默认值，无 display 依赖；规则见 `rules`，文案见 `text`。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AccelLevel {
    #[default]
    Auto,
    Scalar,
    Sse,
    Sse2,
    Sse3,
    Ssse3,
    Sse41,
    Sse42,
    Popcnt,
    Lzcnt,
    Movbe,
    Aes,
    Pclmulqdq,
    Rdrand,
    Rdseed,
    Sha,
    Gfni,
    // AMD 扩展叶档（8000_0001H ECX：bit 21 tbm / bit 6 sse4a）：性能序垫底
    // （X128 钳住用，`target_feature` 小写无点形 `sse4a` 先例，`tbm` 同式）。
    Tbm,
    Sse4a,
    Avx,
    F16c,
    Fma,
    Avx2,
    Bmi1,
    Bmi2,
    Avxvnni,
    Avx512f,
    Avx512cd,
    Avx512dq,
    Avx512bw,
    Avx512vl,
}

impl AccelLevel {
    /// 全集（性能序/加速效率序，`auto` 恒首；`ALL` 下标即滑杆档位 0-30）：
    /// `scalar`(U64) → X128 组（`sse`→`sse42` 整组，再密码/计数类
    /// `popcnt`/`lzcnt`/`movbe`/`aes`/`pclmulqdq`/`rdrand`/`rdseed`/`sha`/`gfni`，
    /// 最后 AMD 专有 `tbm`/`sse4a` 垫底，QEMU `qemu64` 无此叶，显式配了即按
    /// X128 钳住用）→ V128（`avx`/`f16c`）→ Y256（`fma`/`avx2`/`bmi1`/`bmi2`/
    /// `avxvnni`）→ Z512（`avx512f`/`cd`/`dq`/`bw`/`vl`），见 `docs/simd.md`。
    pub(crate) const ALL: [Self; 31] = [
        Self::Auto,
        Self::Scalar,
        Self::Sse,
        Self::Sse2,
        Self::Sse3,
        Self::Ssse3,
        Self::Sse41,
        Self::Sse42,
        Self::Popcnt,
        Self::Lzcnt,
        Self::Movbe,
        Self::Aes,
        Self::Pclmulqdq,
        Self::Rdrand,
        Self::Rdseed,
        Self::Sha,
        Self::Gfni,
        Self::Tbm,
        Self::Sse4a,
        Self::Avx,
        Self::F16c,
        Self::Fma,
        Self::Avx2,
        Self::Bmi1,
        Self::Bmi2,
        Self::Avxvnni,
        Self::Avx512f,
        Self::Avx512cd,
        Self::Avx512dq,
        Self::Avx512bw,
        Self::Avx512vl,
    ];
    /// 主笛卡尔代表集（9 档，不膨胀：性能序时代仍占 9 席，旧口径
    /// `avx512`→`avx512f` 改名后仍占一位；全量 31 档进
    /// `accel_levels_roundtrip` 表，不进主笛卡尔，避 3 倍膨胀）。
    pub(crate) const REPRESENTATIVE: [Self; 9] = [
        Self::Auto,
        Self::Scalar,
        Self::Sse2,
        Self::Ssse3,
        Self::Sse41,
        Self::Sse42,
        Self::Avx,
        Self::Avx2,
        Self::Avx512f,
    ];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scalar => "scalar",
            Self::Sse => "sse",
            Self::Sse2 => "sse2",
            Self::Sse3 => "sse3",
            Self::Ssse3 => "ssse3",
            Self::Sse41 => "sse41",
            Self::Sse42 => "sse42",
            Self::Popcnt => "popcnt",
            Self::Aes => "aes",
            Self::Pclmulqdq => "pclmulqdq",
            Self::Avx => "avx",
            Self::F16c => "f16c",
            Self::Fma => "fma",
            Self::Avx2 => "avx2",
            Self::Bmi1 => "bmi1",
            Self::Bmi2 => "bmi2",
            Self::Lzcnt => "lzcnt",
            Self::Movbe => "movbe",
            Self::Rdrand => "rdrand",
            Self::Rdseed => "rdseed",
            Self::Sha => "sha",
            Self::Gfni => "gfni",
            Self::Avx512f => "avx512f",
            Self::Avx512cd => "avx512cd",
            Self::Avx512dq => "avx512dq",
            Self::Avx512bw => "avx512bw",
            Self::Avx512vl => "avx512vl",
            Self::Avxvnni => "avxvnni",
            Self::Tbm => "tbm",
            Self::Sse4a => "sse4a",
        }
    }
    /// 滑杆档位 0-30（`ALL` 的下标，`Auto=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RenderMode {
    #[default]
    Auto,
    Direct,
    Blt,
}

impl RenderMode {
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Direct => "direct",
            Self::Blt => "blt",
        }
    }
}

/// 多核开关（与固件 `WorkersMode` 同构；Blt 路径恒单线程，忽略本项）。
/// 唯一位置是 `[schedule]`（`[render]` 下的同名键已删，老文件残留直接忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkersMode {
    #[default]
    Auto,
    BspOnly,
    All,
}

impl WorkersMode {
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::BspOnly, Self::All];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::BspOnly => "bsp_only",
            Self::All => "all",
        }
    }
}

/// 多核分工（与固件 `DistMode` 同构；`Tile` = tile 自调度，`Static` = 固定分区，
/// `Dynamic` = 按实测核速加权分行，播中逐轮重算）。
/// 唯一位置是 `[schedule]`（`[render]` 下的同名键已删，老文件残留直接忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DistMode {
    #[default]
    Auto,
    Static,
    Tile,
    Dynamic,
}

impl DistMode {
    pub(crate) const ALL: [Self; 4] = [Self::Auto, Self::Static, Self::Tile, Self::Dynamic];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Static => "static",
            Self::Tile => "tile",
            Self::Dynamic => "dynamic",
        }
    }
}

/// 垂直同步（与固件 `VsyncMode` 同构；`Static` 按预算滑杆封顶，`Half` 实测取半，
/// `Off` 只按播放帧率跑）。
/// 旧 `"on"` 别名已删：老文件写了它即整文件回默认 + 警告（见矩阵 `vsync_legacy_on_rejected`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VsyncMode {
    #[default]
    Auto,
    Static,
    Half,
    Off,
}

impl VsyncMode {
    pub(crate) const ALL: [Self; 4] = [Self::Auto, Self::Static, Self::Half, Self::Off];
    pub(crate) fn file_str(self) -> &'static str {
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
pub(crate) enum BltOpKind {
    #[default]
    BufferToVideo,
    VideoToVideo,
    VideoFill,
    VideoToBltBuffer,
}

impl BltOpKind {
    pub(crate) const ALL: [Self; 4] = [
        Self::BufferToVideo,
        Self::VideoToVideo,
        Self::VideoFill,
        Self::VideoToBltBuffer,
    ];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::BufferToVideo => "buffer_to_video",
            Self::VideoToVideo => "video_to_video",
            Self::VideoFill => "video_fill",
            Self::VideoToBltBuffer => "video_to_blt_buffer",
        }
    }
}

pub(crate) fn default_frames() -> u32 {
    30
}

pub(crate) fn default_fps() -> String {
    String::from("auto")
}

pub(crate) fn default_wallpaper_fps() -> String {
    String::from("15")
}

pub(crate) fn default_wallpaper_preload() -> u32 {
    8
}

pub(crate) fn default_loop_mode() -> LoopMode {
    LoopMode::LoopList
}

pub(crate) fn default_tile_rows() -> u32 {
    64
}

pub(crate) fn default_vsync_fps() -> u32 {
    60
}

pub(crate) fn default_window() -> u32 {
    4
}

pub(crate) fn default_buffers() -> u32 {
    0
}

/// 播放帧率开关（与固件 `parse_fps` 同逻辑：非法值回落 auto）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FpsPolicy {
    Auto,
    Max,
    Capped(u32),
}

pub(crate) fn parse_fps(s: &str) -> FpsPolicy {
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
pub(crate) struct AccelConfig {
    pub(crate) level: AccelLevel,
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
/// `window` 是流式供片窗口（1-32，默认 4；只对 `stream` 生效，
/// `clip` 全预解码、`staged` 单步三段与 `zero_copy` 单缓冲都恒窗口 1，
/// 忽略本项；与固件 `FeedMode` 同构，见 `docs/player.md` 供片节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RenderTune {
    #[serde(default = "default_frames")]
    pub(crate) frames: u32,
    #[serde(default = "default_tile_rows")]
    pub(crate) tile_rows: u32,
    #[serde(default = "default_vsync_fps")]
    pub(crate) vsync_fps: u32,
    #[serde(default = "default_buffers")]
    pub(crate) buffers: u32,
    #[serde(default = "default_window")]
    pub(crate) window: u32,
}

impl Default for RenderTune {
    fn default() -> Self {
        Self {
            frames: default_frames(),
            tile_rows: default_tile_rows(),
            vsync_fps: default_vsync_fps(),
            buffers: default_buffers(),
            window: default_window(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RenderConfig {
    pub(crate) mode: RenderMode,
    pub(crate) vsync: VsyncMode,
    pub(crate) blt_op: BltOpKind,
    pub(crate) tune: RenderTune,
    /// auto 校准记忆（`Auto`/`0` = 未测）：只由固件回写，这里只读展示 + 重测清零。
    #[serde(default)]
    pub(crate) measured_render: RenderMode,
    #[serde(default)]
    pub(crate) measured_workers: WorkersMode,
    #[serde(default)]
    pub(crate) measured_dist: DistMode,
    #[serde(default)]
    pub(crate) measured_vsync: VsyncMode,
    /// half 实测预算（0 = 未测）：固件回写，只读展示 + 重测清零。
    #[serde(default)]
    pub(crate) measured_vsync_fps: u32,
    #[serde(default)]
    pub(crate) measured_cores: u32,
}

/// 片源格式（与固件 `MediaFormat` 同构；小写序列化）。
/// 缺键老配置：play 按 qois、wallpaper 按 raw（见两侧 `default_*_format`）。
/// `hevc`/`avc`/`vp9` 走 MP4 拆轨（石雨堂）或裸流；`h263`/`mpeg2v`/`theora`
/// 走裸流直解（样片见固件 `src/diag/data/`，分发见固件 `video::load_video_clip`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MediaFormat {
    Qois,
    Raw,
    Hevc,
    Vp9,
    H263,
    Mpeg2v,
    Theora,
    Avc,
}

impl MediaFormat {
    pub(crate) const ALL: [Self; 8] = [
        Self::Qois,
        Self::Raw,
        Self::Hevc,
        Self::Vp9,
        Self::H263,
        Self::Mpeg2v,
        Self::Theora,
        Self::Avc,
    ];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Qois => "qois",
            Self::Raw => "raw",
            Self::Hevc => "hevc",
            Self::Vp9 => "vp9",
            Self::H263 => "h263",
            Self::Mpeg2v => "mpeg2v",
            Self::Theora => "theora",
            Self::Avc => "avc",
        }
    }
}

/// 播放列表循环模式（与固件 `LoopMode` 同构；小写蛇形序列化，默认 `loop_list`）。
/// `LoopSingle` = 当前条目循环；`LoopList` = 列表循环；`Once` = 播完即停（返回菜单）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LoopMode {
    LoopSingle,
    #[default]
    LoopList,
    Once,
}

impl LoopMode {
    pub(crate) const ALL: [Self; 3] = [Self::LoopSingle, Self::LoopList, Self::Once];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::LoopSingle => "loop_single",
            Self::LoopList => "loop_list",
            Self::Once => "once",
        }
    }
    /// 滑杆档位 0-2（`ALL` 的下标，`LoopSingle=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(1)
    }
}

/// 播放列表单条（`[[playlist.items]]` / `[[wallpaper.items]]` 唯一键 `path`；
/// 固件按扩展名 + MP4 探轨自动识别格式，失败跳过 + `log::warn!`，不断播）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct PlaylistItem {
    #[serde(default)]
    pub(crate) path: String,
}

/// `[playlist]`：`loop` + `[[playlist.items]]`（顺序即播放序；空列表 = `Err`，无静默菜单）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct PlaylistConfig {
    #[serde(rename = "loop", default = "default_loop_mode")]
    pub(crate) loop_mode: LoopMode,
    #[serde(default)]
    pub(crate) items: Vec<PlaylistItem>,
}

/// `[play] feed`：解码供片模式（与固件 `FeedMode` 同构；`file_str` 即落盘串）。
/// `Clip` = 开机全预解码现路（默认）；`Staged` = 显式三段（解码/搬运/上屏分开）；
/// `Stream` = 窗口边播边解（窗口大小见 `[render.tune] window`）；
/// `ZeroCopy` = 单缓冲即时解码；`Single` = 每帧重读文件 + 重解码（零缓存对照）。
/// 多核（`mp_draw`）/动态调度/壁纸恒为 `clip`（供不上时警告回落，见 hint 文案）。
/// 旧 `[play] pipeline` 表已删：7 老档按 `docs/player.md` 映射走 clip 路径，
/// 老文件残留该键直接忽略（未知键 serde 默认忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedMode {
    #[default]
    Clip,
    Staged,
    Stream,
    ZeroCopy,
    Single,
}

impl FeedMode {
    pub(crate) const ALL: [Self; 5] = [
        Self::Clip,
        Self::Staged,
        Self::Stream,
        Self::ZeroCopy,
        Self::Single,
    ];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Staged => "staged",
            Self::Stream => "stream",
            Self::ZeroCopy => "zero_copy",
            Self::Single => "single",
        }
    }
    /// 滑杆档位 0-4（`ALL` 的下标，`Clip=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

/// 解码线程（与固件 `DecodeThreads` 同构；软解码状态机单核串行，`all` 在有状态
/// 编解码器上只解析 + 警告 + 按 `bsp_only` 跑，见 `docs/player.md` 可行性节）。
/// `qois` 帧级并行可兑现，`raw` 零解码占位（同表八格式全键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecodeThreads {
    #[default]
    BspOnly,
    All,
}

impl DecodeThreads {
    pub(crate) const ALL: [Self; 2] = [Self::BspOnly, Self::All];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::BspOnly => "bsp_only",
            Self::All => "all",
        }
    }
    /// 滑杆档位 0-1（`ALL` 的下标，`BspOnly=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

/// `[decode]` 解码表实现见 `decode_cfg`（`schema` 已到 1000 行上限，子分类拆出；
/// 本处只重导出 `DecodeTable`（`FileConfig.decode` 字段类型，对外路径不变）；
/// `DecodeConfig` 请直引 `decode_cfg`（`rules` 等已直引，避免重导出悬空告警）。
pub(crate) use crate::decode_cfg::DecodeTable;

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），这里只读展示 + 重测清零。
/// `feed` 是解码供片全局默认（`clip` 全预解码，默认，老文件缺键照读；
/// 逐格式 `decode.<fmt>.fetch` 覆盖它，壁纸恒 `clip` 忽略覆盖）。
/// 旧 `format` 键已删（逐条目自动识别）：老文件残留直接忽略（未知键 serde 默认忽略）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PlayConfig {
    #[serde(default = "default_fps")]
    pub(crate) fps: String,
    #[serde(default)]
    pub(crate) measured_max_fps: u32,
    #[serde(default)]
    pub(crate) feed: FeedMode,
}

/// `[boot]`：多卷自动发现开关（与固件 `BootConfig` 同构；全 `true` = 默认全开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BootConfig {
    pub(crate) auto: bool,
    pub(crate) windows: bool,
    pub(crate) linux: bool,
    pub(crate) shell: bool,
    pub(crate) uki: bool,
    pub(crate) macos: bool,
    pub(crate) strict: bool,
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

/// 异步执行器线（与固件 `AsyncExecutor` 同构；只驱动诊断项 `a` 跑哪条线，
/// 不切生产调度）。缺键老配置按 `All` 照读。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AsyncExecutor {
    #[default]
    All,
    Nano,
    Dynamic,
}

impl AsyncExecutor {
    pub(crate) const ALL: [Self; 3] = [Self::All, Self::Nano, Self::Dynamic];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Nano => "nano",
            Self::Dynamic => "dynamic",
        }
    }
}

/// 调度后端（与固件 `ScheduleBackend` 同构）：manual 手写 MP 三路；
/// nano 单核执行器泵（workers/dist 忽略）；dynamic 多核 steal 泵（只直写前台）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ScheduleBackend {
    #[default]
    Manual,
    Nano,
    Dynamic,
}

impl ScheduleBackend {
    pub(crate) const ALL: [Self; 3] = [Self::Manual, Self::Nano, Self::Dynamic];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Nano => "nano",
            Self::Dynamic => "dynamic",
        }
    }
}

/// 核心分工（与固件 `RolesMode` 同构；`Combined` = 全员行分区拷贝，
/// `Split` = AP 解码→槽 + BSP 上传（仅 qois/raw 懒供片，否则警告回落），
/// `Auto` = 符合条件即 split 否则 combined）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RolesMode {
    #[default]
    Auto,
    Combined,
    Split,
}

impl RolesMode {
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::Combined, Self::Split];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Combined => "combined",
            Self::Split => "split",
        }
    }
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Combined => 1,
            Self::Split => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Combined,
            2 => Self::Split,
            _ => Self::Auto,
        }
    }
}

/// `[schedule]`：与固件 `ScheduleConfig` 同构（手写调度独立成矩阵配置）。
/// `cores` 0 = 自动（不限，机型全核），N>0 = 至多 N 核（BSP + 最小编号 N-1 个 AP），钳 0..=64；
/// `roles` 见 `RolesMode`；`measured_workers_n` 0 = 未测（固件首轮 sweep 持久化胜者，
/// GUI 只读展示 + 重测清零）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ScheduleConfig {
    pub(crate) backend: ScheduleBackend,
    pub(crate) workers: WorkersMode,
    pub(crate) dist: DistMode,
    #[serde(default)]
    pub(crate) cores: u32,
    #[serde(default)]
    pub(crate) roles: RolesMode,
    #[serde(default)]
    pub(crate) measured_workers_n: u32,
}

/// `[async]`：与固件 `AsyncConfig` 同构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AsyncConfig {
    pub(crate) executor: AsyncExecutor,
}

/// `[wallpaper] fps`：纯字符串三态（整数老文件不再兼容：`fps = 15` 即整文件
/// 回默认 + 警告，见矩阵 `wallpaper_fps_int_rejected`）。
/// 与固件同口径（`[play] fps` 同构）。
/// `[wallpaper]` 新式画廊列表（与固件 `WallpaperListConfig` 同构）：
/// `enable` 仍是总开关（关 = None，装饰永不 halt）；
/// `loop` + `[[wallpaper.items]]` 逐条目自动识别（顺序即播放序）；
/// `fps` 同播放三态；`preload` 是逐条目封顶（每条截到 `preload`，0 = 不封顶，
/// 8MB/帧 bound）；`popup_freeze` 保留；旧 `format`/`frames` 键已删
/// （老文件残留直接忽略，未知键 serde 默认忽略）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WallpaperListConfig {
    pub(crate) enable: bool,
    #[serde(rename = "loop", default = "default_loop_mode")]
    pub(crate) loop_mode: LoopMode,
    #[serde(default = "default_wallpaper_fps")]
    pub(crate) fps: String,
    #[serde(default = "default_wallpaper_preload")]
    pub(crate) preload: u32,
    #[serde(default = "default_true")]
    pub(crate) popup_freeze: bool,
    #[serde(default)]
    pub(crate) items: Vec<PlaylistItem>,
}

pub(crate) fn default_true() -> bool {
    true
}

impl Default for WallpaperListConfig {
    fn default() -> Self {
        Self {
            enable: false,
            loop_mode: LoopMode::LoopList,
            fps: default_wallpaper_fps(),
            preload: default_wallpaper_preload(),
            popup_freeze: true,
            items: Vec::new(),
        }
    }
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            measured_max_fps: 0,
            feed: FeedMode::Clip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FileConfig {
    pub(crate) version: u32,
    pub(crate) accel: AccelConfig,
    pub(crate) render: RenderConfig,
    pub(crate) play: PlayConfig,
    pub(crate) playlist: PlaylistConfig,
    pub(crate) decode: DecodeTable,
    pub(crate) wallpaper: WallpaperListConfig,
    pub(crate) boot: BootConfig,
    pub(crate) schedule: ScheduleConfig,
    pub(crate) r#async: AsyncConfig,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            version: 1,
            accel: AccelConfig::default(),
            render: RenderConfig::default(),
            play: PlayConfig::default(),
            playlist: PlaylistConfig::default(),
            decode: DecodeTable::default(),
            wallpaper: WallpaperListConfig::default(),
            boot: BootConfig::default(),
            schedule: ScheduleConfig::default(),
            r#async: AsyncConfig::default(),
        }
    }
}

/// 文件 schema 回归：旧文件（无 `[wallpaper]` 表）必须照读，新字段回写不丢。
#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    pub(crate) fn old_file_without_wallpaper_parses() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(!cfg.wallpaper.enable);
        assert_eq!(cfg.wallpaper.fps, default_wallpaper_fps());
        assert_eq!(cfg.wallpaper.preload, default_wallpaper_preload());
        assert_eq!(cfg.schedule.dist, DistMode::Auto);
        assert_eq!(cfg.render.measured_dist, DistMode::Auto);
    }

    #[test]
    pub(crate) fn tune_fields_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.render.tune.frames = 45;
        cfg.render.tune.tile_rows = 32;
        cfg.render.tune.vsync_fps = 90;
        cfg.render.tune.buffers = 5;
        cfg.render.tune.window = 7;
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.render.tune.frames, 45);
        assert_eq!(back.render.tune.tile_rows, 32);
        assert_eq!(back.render.tune.vsync_fps, 90);
        assert_eq!(back.render.tune.buffers, 5);
        assert_eq!(back.render.tune.window, 7);
    }

    #[test]
    pub(crate) fn tune_missing_keys_use_defaults() {
        // 比 wallpaper 测试更老的文件：连 [render.tune] 都没有。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.tune.frames, default_frames());
        assert_eq!(cfg.render.tune.tile_rows, default_tile_rows());
        assert_eq!(cfg.render.tune.vsync_fps, default_vsync_fps());
        assert_eq!(cfg.render.tune.buffers, default_buffers());
        assert_eq!(cfg.render.tune.window, default_window());
    }

    #[test]
    pub(crate) fn vsync_half_roundtrip() {
        // 新档位：half 照读照写，预算回写不丢。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"direct\"\nvsync = \"half\"\n[play]\nfps = \"auto\"\n";
        let mut cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.vsync, VsyncMode::Half);
        cfg.render.measured_vsync_fps = 42;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.render.vsync, VsyncMode::Half);
        assert_eq!(back.render.measured_vsync_fps, 42);
    }

    #[test]
    pub(crate) fn vsync_legacy_on_rejected() {
        // 旧 `"on"` 别名已删：整文件解析失败 → 调用方走全默认 + 警告（不断旧配置是固件侧行为）。
        assert!(toml::from_str::<FileConfig>("[render]\nvsync = \"on\"\n").is_err());
    }

    #[test]
    pub(crate) fn dist_dynamic_roundtrip() {
        // 新档位：显式 dynamic 照读照写，measured 同理（旧文件无该值不受影响）。
        // workers/dist 唯一位置是 `[schedule]`（`[render]` 下同名键已删，残留直接忽略）。
        let text = "version = 1\n[render]\nmode = \"direct\"\n[schedule]\nworkers = \"all\"\ndist = \"dynamic\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.schedule.dist, DistMode::Dynamic);
        assert_eq!(cfg.schedule.workers, WorkersMode::All);
        let mut cfg = cfg;
        cfg.render.measured_dist = DistMode::Dynamic;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.schedule.dist, DistMode::Dynamic);
        assert_eq!(back.render.measured_dist, DistMode::Dynamic);
    }

    #[test]
    pub(crate) fn wallpaper_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.wallpaper.enable = true;
        cfg.wallpaper.fps = String::from("20");
        cfg.wallpaper.loop_mode = LoopMode::Once;
        cfg.wallpaper.preload = 4;
        cfg.wallpaper.popup_freeze = false;
        cfg.wallpaper.items = vec![
            PlaylistItem {
                path: String::from("wallpaper/wall.mp4"),
            },
            PlaylistItem {
                path: String::from("wallpaper/wall.qois"),
            },
        ];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"once\""),
            "loop key missing in {text}"
        );
        assert!(
            text.contains("[[wallpaper.items]]"),
            "items missing in {text}"
        );
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert!(back.wallpaper.enable);
        assert_eq!(back.wallpaper.fps, "20");
        assert_eq!(back.wallpaper.loop_mode, LoopMode::Once);
        assert_eq!(back.wallpaper.preload, 4);
        assert!(!back.wallpaper.popup_freeze);
        assert_eq!(back.wallpaper.items.len(), 2);
        assert_eq!(back, cfg);
    }

    #[test]
    pub(crate) fn playlist_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.playlist.loop_mode = LoopMode::LoopSingle;
        cfg.playlist.items = vec![
            PlaylistItem {
                path: String::from("1080p/video.mp4"),
            },
            PlaylistItem {
                path: String::from("1080p/video.hevc"),
            },
            PlaylistItem {
                path: String::from("1080p/video.qois"),
            },
        ];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"loop_single\""),
            "loop key missing in {text}"
        );
        assert!(
            text.contains("[[playlist.items]]"),
            "items missing in {text}"
        );
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.playlist.loop_mode, LoopMode::LoopSingle);
        assert_eq!(back.playlist.items.len(), 3);
        assert_eq!(back.playlist.items[0].path, "1080p/video.mp4");
        assert_eq!(back, cfg);
        // 缺表老文件：空列表 + loop_list。
        let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
        assert!(legacy.playlist.items.is_empty());
        assert_eq!(legacy.playlist.loop_mode, LoopMode::LoopList);
        // 旧 `[play] format` / `[wallpaper] format,frames` 残留直接忽略，不断解析。
        let old: FileConfig = toml::from_str(
            "version = 1\n[play]\nfps = \"max\"\nformat = \"raw\"\n[wallpaper]\nformat = \"qois\"\nframes = 30\n",
        )
        .unwrap();
        assert_eq!(old.play.fps, "max");
        assert!(old.playlist.items.is_empty());
        assert!(old.wallpaper.items.is_empty());
    }
}
