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
    #[serde(alias = "avx512")]
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
/// 固件侧已搬家到 `[schedule]`；`[render]` 下是 legacy 别名（老文件照读，
/// 新文件 GUI 只写 `[schedule]`；见 `coerce_loaded` 迁移）。
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
/// 搬家说明同 `WorkersMode`（`[schedule] dist` 为准）。
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
/// `Off` 只按播放帧率跑；旧文件 `"on"` 按 `static` 照读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum VsyncMode {
    #[default]
    Auto,
    #[serde(alias = "on")]
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
    pub(crate) workers: WorkersMode,
    pub(crate) dist: DistMode,
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

    /// 是否走 MP4 容器（GUI 主行收拢为一颗“MP4 视频”按钮，子行再单选三轨；
    /// 固件 schema 不变，落盘仍是 `hevc`/`avc`/`vp9` 小写串）。
    pub(crate) fn is_mp4(self) -> bool {
        matches!(self, Self::Hevc | Self::Avc | Self::Vp9)
    }
}

pub(crate) fn default_play_format() -> MediaFormat {
    MediaFormat::Qois
}

pub(crate) fn default_wallpaper_format() -> MediaFormat {
    MediaFormat::Raw
}

/// `[play] pipeline`：播放实现手动切换（与固件 `PlayPipeline` 同构）。
/// `Clip` = 预解码现路（默认）；其余 7 档是修复后的老实现循环，只读 qois。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlayPipeline {
    #[default]
    Clip,
    Staged,
    ZeroCopy,
    Stream,
    Single,
    LegacyBlt,
    LegacyDirect,
    LegacyU64,
}

impl PlayPipeline {
    pub(crate) const ALL: [Self; 8] = [
        Self::Clip,
        Self::Staged,
        Self::ZeroCopy,
        Self::Stream,
        Self::Single,
        Self::LegacyBlt,
        Self::LegacyDirect,
        Self::LegacyU64,
    ];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Staged => "staged",
            Self::ZeroCopy => "zero_copy",
            Self::Stream => "stream",
            Self::Single => "single",
            Self::LegacyBlt => "legacy_blt",
            Self::LegacyDirect => "legacy_direct",
            Self::LegacyU64 => "legacy_u64",
        }
    }
}

/// `[play] feed`：解码供片模式（与固件 `FeedMode` 同构；`file_str` 即落盘串）。
/// `Clip` = 开机全预解码现路（默认）；`Staged` = 显式三段（解码/搬运/上屏分开）；
/// `Stream` = 窗口边播边解（窗口大小见 `[render.tune] window`）；
/// `ZeroCopy` = 单缓冲即时解码（无二级缓存）。
/// 多核（`mp_draw`）/动态调度/壁纸恒为 `clip`（供不上时警告回落，见 hint 文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedMode {
    #[default]
    Clip,
    Staged,
    Stream,
    ZeroCopy,
}

impl FeedMode {
    pub(crate) const ALL: [Self; 4] = [Self::Clip, Self::Staged, Self::Stream, Self::ZeroCopy];
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Staged => "staged",
            Self::Stream => "stream",
            Self::ZeroCopy => "zero_copy",
        }
    }
    /// 滑杆档位 0-3（`ALL` 的下标，`Clip=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），这里只读展示 + 重测清零。
/// `format` 切片源：qois 读 `1080p/video.qois`，raw 读 `1080p/frameNNNN.raw`，
/// hevc 读 `1080p/video.hevc`（Annex-B 软解），vp9 读 `1080p/video.ivf`（软解），
/// h263/mpeg2v/theora 格式可选、解码后继（固件明确报错，不断解析）。
/// `pipeline` 切播放实现：clip 是预解码现路（默认），其余 7 档是老实现循环。
/// `feed` 切解码供片：clip 全预解码（默认，老文件缺键照读）；
/// staged/stream/zero_copy 只供 qois（mp/dynamic/壁纸恒 clip）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PlayConfig {
    #[serde(default = "default_fps")]
    pub(crate) fps: String,
    #[serde(default)]
    pub(crate) measured_max_fps: u32,
    #[serde(default = "default_play_format")]
    pub(crate) format: MediaFormat,
    #[serde(default)]
    pub(crate) pipeline: PlayPipeline,
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

/// `[schedule]`：与固件 `ScheduleConfig` 同构（手写调度独立成矩阵配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ScheduleConfig {
    pub(crate) backend: ScheduleBackend,
    pub(crate) workers: WorkersMode,
    pub(crate) dist: DistMode,
}

/// `[async]`：与固件 `AsyncConfig` 同构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AsyncConfig {
    pub(crate) executor: AsyncExecutor,
}

/// `[wallpaper] fps` 反序列化：字符串三态 + 兼容老文件整数（固件侧同款，
/// host/guest 隔离故各一份，见固件 `config.rs::de_wallpaper_fps`）。
fn de_wallpaper_fps<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = String;
        fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            f.write_str("wallpaper fps as string or integer")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }
    }
    d.deserialize_any(V)
}

/// `[wallpaper]`：与固件 `WallpaperConfig` 同构。无校准记忆，纯开关。
/// `format` 切片源：raw 读 `wallpaper/frameNNNN.raw`，qois 读 `wallpaper/wall.qois`。
/// `popup_freeze`：选项弹窗时冻结壁纸（false 则行区继续播）。
/// `fps` 三态（与 `[play] fps` 同构）：`"auto"` 最左 / `"max"` 最右 / 中间 `1`-`240`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WallpaperConfig {
    pub(crate) enable: bool,
    #[serde(
        default = "default_wallpaper_fps",
        deserialize_with = "de_wallpaper_fps"
    )]
    pub(crate) fps: String,
    #[serde(default)]
    pub(crate) frames: u32,
    #[serde(default = "default_wallpaper_preload")]
    pub(crate) preload: u32,
    #[serde(default = "default_wallpaper_format")]
    pub(crate) format: MediaFormat,
    #[serde(default = "default_true")]
    pub(crate) popup_freeze: bool,
}

pub(crate) fn default_true() -> bool {
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
            pipeline: PlayPipeline::Clip,
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
    pub(crate) wallpaper: WallpaperConfig,
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
            wallpaper: WallpaperConfig::default(),
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
        assert_eq!(cfg.render.dist, DistMode::Auto);
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
    pub(crate) fn vsync_half_roundtrip_and_on_alias() {
        // 新档位：half 照读照写，预算回写不丢。
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"direct\"\nworkers = \"all\"\nvsync = \"half\"\n[play]\nfps = \"auto\"\n";
        let mut cfg: FileConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.render.vsync, VsyncMode::Half);
        cfg.render.measured_vsync_fps = 42;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.render.vsync, VsyncMode::Half);
        assert_eq!(back.render.measured_vsync_fps, 42);
        // 旧文件 `"on"` 按 static 照读（固件同 alias，不断旧配置）。
        let old: FileConfig = toml::from_str("[render]\nvsync = \"on\"\n").unwrap();
        assert_eq!(old.render.vsync, VsyncMode::Static);
    }

    #[test]
    pub(crate) fn dist_dynamic_roundtrip() {
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
    pub(crate) fn wallpaper_roundtrip() {
        let mut cfg = FileConfig::default();
        cfg.wallpaper.enable = true;
        cfg.wallpaper.fps = String::from("20");
        cfg.wallpaper.frames = 30;
        cfg.wallpaper.format = MediaFormat::Qois;
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert!(back.wallpaper.enable);
        assert_eq!(back.wallpaper.fps, "20");
        assert_eq!(back.wallpaper.frames, 30);
        assert_eq!(back.wallpaper.format, MediaFormat::Qois);
    }
}
