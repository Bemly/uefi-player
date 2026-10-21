//! `player.toml` schema 类型（`src/config` 的 schema 半；运行时生效值/原子存储见 `super`）。
//!
//! 与 `tools/config-gui/src/schema.rs` 同构（字段名/枚举字符串一致，改动双边同步）。

use alloc::string::{String, ToString};

use serde::{Deserialize, Serialize};

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
    pub(crate) fn as_u8(self) -> u8 {
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
    pub(crate) fn from_u8(v: u8) -> Self {
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

/// `[accel.decode]` 单家解码 SIMD 开关（`hevc`/`vp9`/`h263` 三键，各自独立）。
/// - `Auto`（默认，老文件缺键照此）：跟随全局 `level`，`scalar`=关、其余=开；
/// - `Scalar`：强制标量路（哪怕全局开了高档）；
/// - `Max`：强制轮子上限（哪怕全局是 `scalar`，取硬件探到的最高可用）。
/// 注意这不是第二套 ISA 档位：`vp9`/`h263` 只有开关语义（开=轮子 max，
/// 关=标量双胞胎），`hevc` 开时取的上限仍是既有映射
/// （`copy_width`/`hw_max` → `Baseline`/`Sse41`/`Avx2`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DecodeSimd {
    #[default]
    Auto,
    Scalar,
    Max,
}

impl DecodeSimd {
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Scalar => 1,
            Self::Max => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Scalar,
            2 => Self::Max,
            _ => Self::Auto,
        }
    }
}

/// `[accel.decode]`：三家解码器各一键（缺表/缺键全按 `Auto` 照读，不断旧配置）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccelDecode {
    pub hevc: DecodeSimd,
    pub vp9: DecodeSimd,
    pub h263: DecodeSimd,
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
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Direct => 1,
            Self::Blt => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Direct,
            2 => Self::Blt,
            _ => Self::Auto,
        }
    }
}

/// 多核开关：`Auto` = 校准矩阵实测（BSP 单核直写 vs 全核并行），只在
/// `render.mode` 允许直写时有意义（Blt 路径恒单线程，忽略本项）。
///
/// 新位置是 `[schedule] workers`；这里是 legacy 别名：`[schedule]` 表存在即忽略，
/// 缺表老文件继续认这里的值（一次性，不写回）。
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
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::BspOnly => 1,
            Self::All => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
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
///
/// 新位置是 `[schedule] dist`；这里是 legacy 别名（规则同 `WorkersMode`）。
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
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Static => 1,
            Self::Tile => 2,
            Self::Dynamic => 3,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Static,
            2 => Self::Tile,
            3 => Self::Dynamic,
            _ => Self::Auto,
        }
    }
}

/// 异步执行器线（`uefi-async` 两条堆线，诊断项 `a` 跑哪条）：
/// - `All`（默认）：nano + dynamic 都跑，各自断言；
/// - `Nano`：只跑单核借用节点线（链表轮询，无跨核原语）；
/// - `Dynamic`：只跑多核 work-stealing 线（spawn/cancel/retune + steal）。
/// 只管诊断覆盖，不切生产调度（播片/壁纸仍是手写 MP，迁移另立项）。
/// 缺键老配置按 `All` 照读。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AsyncExecutor {
    #[default]
    All,
    Nano,
    Dynamic,
}

impl AsyncExecutor {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::All => 0,
            Self::Nano => 1,
            Self::Dynamic => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Nano,
            2 => Self::Dynamic,
            _ => Self::All,
        }
    }
}

/// `[async]`：目前只有诊断项消费（`async_exec` 按档跑线），缺表缺键全默认。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AsyncConfig {
    pub executor: AsyncExecutor,
}

/// 调度后端（`[schedule] backend`，生产路径走哪套调度）：
/// - `Manual`（默认）：手写 MP（`mp_draw` 常驻 AP + 世代同步；`blt/direct_loop`
///   单核泵），`workers`/`dist` 生效，行为与搬家前逐行一致；
/// - `Nano`：单核执行器泵（一个永不结束的 pump task：取帧 → `upload_frame` →
///   限速/掉帧），`render` 全 honoring（blt/direct/影子都走），`workers`/`dist` 忽略；
/// - `Dynamic`：多核 steal 泵（按核分带，BSP 每帧 spawn，AP steal；BSP 独判掉帧，
///   确定性与手写一致）：只直写前台。`render=blt`（Blt 引擎单线程）/ 无 MP /
///   `tune.buffers>0`（影子环 v1 不支持）时 warn + 回退（前两者回 nano，后者回 manual）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleBackend {
    #[default]
    Manual,
    Nano,
    Dynamic,
}

impl ScheduleBackend {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::Manual => 0,
            Self::Nano => 1,
            Self::Dynamic => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Nano,
            2 => Self::Dynamic,
            _ => Self::Manual,
        }
    }
}

/// `[schedule]`：手写调度独立成矩阵配置（P2.5）。`workers`/`dist` 从 `[render]`
/// 搬家至此；缺表老文件继续认 `[render]` 旧键（`load` 一次性 fallback + 警告，
/// 不写回）。`measured_*` 记忆仍在 `[render]`（校准维度未变）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScheduleConfig {
    pub backend: ScheduleBackend,
    pub workers: WorkersMode,
    pub dist: DistMode,
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
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Static => 1,
            Self::Off => 2,
            Self::Half => 3,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
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
    /// 细粒度解码开关（`[accel.decode]` 三键；缺表缺键全 `Auto`=跟随 `level`）。
    #[serde(default)]
    pub decode: AccelDecode,
}

pub(crate) const fn default_frames() -> u32 {
    30
}

pub(crate) const fn default_tile_rows() -> u32 {
    64
}

pub(crate) const fn default_buffers() -> u32 {
    0
}

pub(crate) const fn default_vsync_fps() -> u32 {
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
    /// legacy 别名（见 `WorkersMode`）：`[schedule]` 表存在即忽略。
    pub workers: WorkersMode,
    /// legacy 别名（见 `DistMode`）：`[schedule]` 表存在即忽略。
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
    pub schedule: ScheduleConfig,
    pub r#async: AsyncConfig,
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
            schedule: ScheduleConfig::default(),
            r#async: AsyncConfig::default(),
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

pub(crate) const fn default_true() -> bool {
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

pub(crate) fn default_wallpaper_fps() -> String {
    String::from("15")
}

pub(crate) const fn default_wallpaper_preload() -> u32 {
    8
}

/// `[wallpaper] fps` 反序列化：字符串三态（`"auto"`/`"max"`/`"15"`）+
/// 兼容老文件整数（`fps = 15` 照读为 `"15"`，不断旧配置）。
/// `toml` 标量经 `deserialize_any` 进 `visit_i64/u64/str`，no_std 可用。
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
            Ok(String::from(v))
        }
    }
    d.deserialize_any(V)
}

/// `[wallpaper]`：动态壁纸开关（P4）。片源双格式（`format` 矩阵，见 `MediaFormat`）：
/// - `raw`（默认）：`wallpaper/frameNNNN.raw`，BGRA 直存，首版定死 1920x1080；
/// - `qois`：`wallpaper/wall.qois`（与视频同格式），开机预解码。
/// `popup_freeze`：选项弹窗弹出时冻结壁纸（默认 true；false 则行区继续播，
/// 选项画在文字区，见 `idle.rs`）。缺键老配置按默认（关/raw/冻结）处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WallpaperConfig {
    pub enable: bool,
    /// 背景循环帧率三态（与 `[play] fps` 同构：`"auto"` 最左 / `"max"` 最右 /
    /// 中间 `1`-`240` 显式封顶；默认 `"15"`，老文件整数照读）。
    #[serde(
        default = "default_wallpaper_fps",
        deserialize_with = "de_wallpaper_fps"
    )]
    pub fps: String,
    /// B 过渡封顶帧数（0 = 全播预载集）。
    #[serde(default)]
    pub frames: u32,
    /// 预载帧数上限（内存兜底；单帧 1920x1080x4 ≈ 8MB）。
    /// 0 = 不封顶（和播片一样拉满：有多少读多少，长动画注意内存）。
    #[serde(default = "default_wallpaper_preload")]
    pub preload: u32,
    /// 片源格式（默认 raw，老文件照读）。
    #[serde(default = "default_wallpaper_format")]
    pub format: MediaFormat,
    /// 选项弹窗时冻结壁纸（默认 true；false 则行区继续播）。
    #[serde(default = "default_true")]
    pub popup_freeze: bool,
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

/// 片源格式矩阵（播放器 + 壁纸共用，`Clip` 统一解码）：
/// - `Qois`：QOI 压缩序列（省 U 盘，费解码）；
/// - `Raw`：BGRA 裸帧序列（零解码直搬，费显存/体积）。
/// 非法值整个文件回默认 + 警告（与其它枚举档位同规则，不单独兜底）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaFormat {
    Qois,
    Raw,
}

pub(crate) const fn default_play_format() -> MediaFormat {
    MediaFormat::Qois
}

pub(crate) const fn default_wallpaper_format() -> MediaFormat {
    MediaFormat::Raw
}

impl MediaFormat {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            MediaFormat::Qois => 0,
            MediaFormat::Raw => 1,
        }
    }

    pub(crate) fn from_u8(v: u8) -> MediaFormat {
        match v {
            1 => MediaFormat::Raw,
            _ => MediaFormat::Qois,
        }
    }
}

fn default_fps() -> String {
    String::from("auto")
}

/// 播放管线（`[play] pipeline`，手动切换播放实现）：
/// - `Clip`（默认）：`Clip` 全量预解码 + 现行三路/执行器后端（`schedule.backend` 生效）；
/// - `Staged`：流式三段解码（QOI→Raw→Blt，旧 `draw_all_mem` 循环）；
/// - `ZeroCopy`：流式零拷贝解码（直解进 Blt 缓冲 + R/B 交换，旧 `draw_all_mem_zero_copy` 循环）；
/// - `Stream`：真文件流式（边读边解，旧 `draw` 循环，不全量进内存）；
/// - `Single`：单帧反复（旧 `draw_once` 循环，每帧重读文件，仅对照）；
/// - `LegacyBlt`/`LegacyDirect`/`LegacyU64`：`Clip` 预解码 + 老上传循环
///   （Blt 整推 / BSP 直写 / u64 步进直写，不走 render 矩阵与执行器后端）。
/// 非 `Clip` 管线只读 qois 片源（`format = "raw"` 时 warn 回落 `Clip`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlayPipeline {
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
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::Clip => 0,
            Self::Staged => 1,
            Self::ZeroCopy => 2,
            Self::Stream => 3,
            Self::Single => 4,
            Self::LegacyBlt => 5,
            Self::LegacyDirect => 6,
            Self::LegacyU64 => 7,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Staged,
            2 => Self::ZeroCopy,
            3 => Self::Stream,
            4 => Self::Single,
            5 => Self::LegacyBlt,
            6 => Self::LegacyDirect,
            7 => Self::LegacyU64,
            _ => Self::Clip,
        }
    }
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
/// 只由固件实测回写（0 = 未测），GUI 只读展示。`format` 切片源：
/// `qois`（默认，`1080p/video.qois`）/ `raw`（`1080p/frameNNNN.raw` 序列）。
/// `pipeline` 切播放实现（默认 `clip`，老文件缺键照读；非 clip 只读 qois）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayConfig {
    #[serde(default = "default_fps")]
    pub fps: String,
    #[serde(default)]
    pub measured_max_fps: u32,
    /// 片源格式（默认 qois，老文件照读）。
    #[serde(default = "default_play_format")]
    pub format: MediaFormat,
    /// 播放管线（默认 clip，老文件照读）。
    #[serde(default)]
    pub pipeline: PlayPipeline,
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fps: default_fps(),
            measured_max_fps: 0,
            format: MediaFormat::Qois,
            pipeline: PlayPipeline::Clip,
        }
    }
}
