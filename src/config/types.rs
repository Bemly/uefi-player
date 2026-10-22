//! `player.toml` schema 类型（`src/config` 的 schema 半；运行时生效值/原子存储见 `super`）。
//!
//! 与 `tools/config-gui/src/schema.rs` 同构（字段名/枚举字符串一致，改动双边同步）。

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// 指令集加速档位。`Auto` = 运行时探 CPUID/XCR0 取最高可用。
/// 全集按加速效率/性能排（`as_u8` 下标即滑杆档位）：`auto` 恒首（0）；
/// 随后 `scalar`（U64），X128 组（`sse`→`sse42` 整组，再密码/计数类
/// `popcnt`/`lzcnt`/`movbe`/`aes`/`pclmulqdq`/`rdrand`/`rdseed`/`sha`/`gfni`，
/// 最后 AMD 专有 `tbm`/`sse4a` 垫底），V128（`avx`/`f16c`），Y256
/// （`fma`/`avx2`/`bmi1`/`bmi2`/`avxvnni`），Z512
/// （`avx512f`/`cd`/`dq`/`bw`/`vl`）；共 31 档。
/// `tbm`/`sse4a` 为 AMD 专有（扩展叶 8000_0001H ECX bit 21/6，
/// QEMU `qemu64` 无此叶，显式配了即按 X128 钳住用，见 `docs/simd.md`）。
/// 老文件 `"avx512"` 不再是别名：非法值整文件回默认 + 警告（无别名/回退，
/// 见 `super::parse`），与 `VsyncMode::Static` 的旧 `"on"`、壁纸整数 `fps` 同规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccelLevel {
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
    // AMD 扩展叶档（8000_0001H ECX：bit 21 tbm / bit 6 sse4a，
    // `target_feature` 小写无点形已有 `sse4a` 先例，`tbm` 同式）：
    // 性能序垫底（X128 钳住用，见 `docs/simd.md`）。
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
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Scalar => 1,
            Self::Sse => 2,
            Self::Sse2 => 3,
            Self::Sse3 => 4,
            Self::Ssse3 => 5,
            Self::Sse41 => 6,
            Self::Sse42 => 7,
            Self::Popcnt => 8,
            Self::Lzcnt => 9,
            Self::Movbe => 10,
            Self::Aes => 11,
            Self::Pclmulqdq => 12,
            Self::Rdrand => 13,
            Self::Rdseed => 14,
            Self::Sha => 15,
            Self::Gfni => 16,
            Self::Tbm => 17,
            Self::Sse4a => 18,
            Self::Avx => 19,
            Self::F16c => 20,
            Self::Fma => 21,
            Self::Avx2 => 22,
            Self::Bmi1 => 23,
            Self::Bmi2 => 24,
            Self::Avxvnni => 25,
            Self::Avx512f => 26,
            Self::Avx512cd => 27,
            Self::Avx512dq => 28,
            Self::Avx512bw => 29,
            Self::Avx512vl => 30,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Scalar,
            2 => Self::Sse,
            3 => Self::Sse2,
            4 => Self::Sse3,
            5 => Self::Ssse3,
            6 => Self::Sse41,
            7 => Self::Sse42,
            8 => Self::Popcnt,
            9 => Self::Lzcnt,
            10 => Self::Movbe,
            11 => Self::Aes,
            12 => Self::Pclmulqdq,
            13 => Self::Rdrand,
            14 => Self::Rdseed,
            15 => Self::Sha,
            16 => Self::Gfni,
            17 => Self::Tbm,
            18 => Self::Sse4a,
            19 => Self::Avx,
            20 => Self::F16c,
            21 => Self::Fma,
            22 => Self::Avx2,
            23 => Self::Bmi1,
            24 => Self::Bmi2,
            25 => Self::Avxvnni,
            26 => Self::Avx512f,
            27 => Self::Avx512cd,
            28 => Self::Avx512dq,
            29 => Self::Avx512bw,
            30 => Self::Avx512vl,
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
/// 唯一位置是 `[schedule] workers`；`[render]` 下旧键已删除（未知键忽略，
/// 不再照读；见 `ScheduleConfig`）。
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

/// 多核角色分工（`[schedule] roles`，只管 manual 行路 vs 槽路走哪条；`auto` 恒
/// 等价现状：槽路准入（qois/raw + 零拷系供片 + 多核）即分、否则合）：
/// - `Combined`（合）：全部选中核行分区拷屏（`mp_draw` 行路，BSP 也拷自己份）；
/// - `Split`（分）：AP 解码进自家槽环（`SlotRing`）+ BSP 按序整推（`mp_slots` 槽路；
///   仅帧独立且零拷系供片（见 `slots_feed_eligible`）+ 选中核数 > 1 时可进，
///   否则 warn + 回落合，不进槽路）；
/// - `Auto`（默认）：准入即分（现状行为），否则合。
/// 无别名（非法值整文件回默认 + 警告，与其它枚举档位同规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RolesMode {
    #[default]
    Auto,
    Combined,
    Split,
}

impl RolesMode {
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
    /// 配置文件拼写（serde 小写同形，供串口 `sched:` 行）。
    pub(crate) const fn file_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Combined => "combined",
            Self::Split => "split",
        }
    }
}

/// 帧搬运位宽（P2 真分发的落点）：`accel.level` 只管“档位语义”，
/// 真正上屏/播放走这个位宽。SSE 系同宽 128（标量/密码类档按基线下限同宽处理），
/// AVX/F16C 是 VEX-128，AVX2/FMA/BMI/AVXVNNI 是 256，AVX512 系是 512。
/// 档位→位宽映射见 `super::level_width`。
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
/// 新位置是 `[schedule] dist`；`[render]` 下旧键已删除（未知键忽略，
/// 不再照读；见 `ScheduleConfig`）。
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
/// 不写回）。`measured_*` 记忆仍在 `[render]`（校准维度未变），另见
/// `[schedule] measured_workers_n`（核数记忆，`[render] measured_workers`
/// 只留三态做展示兼容）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScheduleConfig {
    pub backend: ScheduleBackend,
    pub workers: WorkersMode,
    pub dist: DistMode,
    /// 用核预算（0 = 不限，随 workers/记忆走；N > 0 = 至多用 N 核：
    /// BSP + 最小编号 N-1 个 AP，见 `crew` 模块头 BSP-id 假设）。
    /// 与 workers 取交集：`bsp_only` 恒单核；其余先钳预算。0..=64（超限钳 64）。
    #[serde(default)]
    pub cores: u32,
    /// 角色分工（默认 `auto` = 准入即分；`combined` 恒行路；`split` 仅准入时槽路，
    /// 否则 warn + 回落行路，见 `effective_roles`）。
    #[serde(default)]
    pub roles: RolesMode,
    /// 核数记忆（0 = 未测，首启 auto sweep 后回写；`[render] measured_workers`
    /// 三态同步写回，只做展示兼容）。
    #[serde(default)]
    pub measured_workers_n: u32,
}

/// 垂直同步（软件帧预算，GOP 无硬件 vsync）：`Static` = 按 `tune.vsync_fps`
/// 固定封顶；`Half` = 开机实测冠军路最高帧率，取半稳定跑并回写
/// `measured_vsync_fps`（下次启动复用）；`Auto` = 按 `tune.vsync_fps` 预算探测
/// 冠军路，零超期则开（封顶同预算）否则关；`Off` = 只按 `play.fps` 跑。
/// 旧 `"on"` 不再是别名：非法值整文件回默认 + 警告（无别名/回退）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VsyncMode {
    #[default]
    Auto,
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

pub(crate) const fn default_window() -> u32 {
    4
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
    /// 流式供片窗口（`[play] feed = "stream"` 的解码帧缓存数，1-32，默认 4）：
    /// 边播边解，游标之后淘汰。其余 feed 档不用它（clip 全预解码 /
    /// staged 单步三段 / zero_copy 单复用缓冲）。显式旋钮（`buffers` 同口径，
    /// 不进矩阵 bench）；缺键老文件按 4 照读。
    #[serde(default = "default_window")]
    pub window: u32,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RenderConfig {
    pub mode: RenderMode,
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
    pub playlist: PlaylistConfig,
    pub wallpaper: WallpaperListConfig,
    pub boot: BootConfig,
    pub schedule: ScheduleConfig,
    pub r#async: AsyncConfig,
    pub decode: DecodeTable,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            accel: AccelConfig::default(),
            render: RenderConfig::default(),
            play: PlayConfig::default(),
            playlist: PlaylistConfig::default(),
            wallpaper: WallpaperListConfig::default(),
            boot: BootConfig::default(),
            schedule: ScheduleConfig::default(),
            r#async: AsyncConfig::default(),
            decode: DecodeTable::default(),
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

/// 播放列表循环模式（`[playlist] loop` + `[wallpaper] loop` 共用，serde 小写下划线）：
/// - `LoopSingle`：重播当前项（视频播完重载同项，画廊只取首项）；
/// - `LoopList`（默认）：播完回绕；
/// - `Once`：播完即停（视频回菜单，画廊末帧保持）。
///
/// 非法值整文件回默认 + 警告（与其它枚举档位同规则，无别名/回退）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    LoopSingle,
    #[default]
    LoopList,
    Once,
}

impl LoopMode {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::LoopSingle => 0,
            Self::LoopList => 1,
            Self::Once => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::LoopSingle,
            2 => Self::Once,
            _ => Self::LoopList,
        }
    }
    /// 配置文件拼写（serde snake_case 同形，供串口 `playlist:`/`wallpaper: gallery` 行）。
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LoopSingle => "loop_single",
            Self::LoopList => "loop_list",
            Self::Once => "once",
        }
    }
}

/// 播放列表条目（`[[playlist.items]]` / `[[wallpaper.items]]` 单键：片源路径，
/// 斜杠分隔，固件侧统一转反斜杠打开；格式按路径自动检测，见 `play::detect_clip_format`）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlaylistItem {
    pub path: String,
}

/// `[playlist]`：播放列表（`loop` + 条目表；`loop` 是 Rust 关键字故字段名 `loop_mode` + rename）。
/// 缺表缺键全默认（空列表，播片直接 Err，不断 boot）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlaylistConfig {
    #[serde(rename = "loop")]
    pub loop_mode: LoopMode,
    #[serde(default)]
    pub items: Vec<PlaylistItem>,
}

/// `[wallpaper] fps`：字符串三态（`"auto"`/`"max"`/`"15"`）。整数值不再兼容：
/// 非字符串即整文件回默认 + 警告（无别名/回退，与 `accel.level` 旧 `"avx512"`、
/// `vsync` 旧 `"on"` 同规则）。
///
/// `[wallpaper]`：动态壁纸画廊（`enable` 总开关 + `loop` 循环 + 条目表；
/// `format`/`frames` 旧键已删除，残留按 serde 默认忽略，无别名；
/// 每项按路径自动检测，与播放列表同一 `detect_clip_format`，raw 走画廊目录扫描；
/// `preload` 按项封顶，B 过渡长度由项数代替 `frames`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WallpaperListConfig {
    pub enable: bool,
    #[serde(rename = "loop")]
    pub loop_mode: LoopMode,
    /// 背景循环帧率三态（与 `[play] fps` 同构：`"auto"` 最左 / `"max"` 最右 /
    /// 中间 `1`-`240` 显式封顶；默认 `"15"`）。
    #[serde(default = "default_wallpaper_fps")]
    pub fps: String,
    /// 预载帧数上限（内存兜底；单帧 1920x1080x4 ≈ 8MB）。
    /// 0 = 不封顶（和播片一样拉满：有多少读多少，长动画注意内存）。
    #[serde(default = "default_wallpaper_preload")]
    pub preload: u32,
    /// 选项弹窗时冻结壁纸（默认 true；false 则行区继续播）。
    #[serde(default = "default_true")]
    pub popup_freeze: bool,
    #[serde(default)]
    pub items: Vec<PlaylistItem>,
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

/// `frameNNNN.raw` 文件名规则（播放列表 raw 项 / 画廊 raw 项共用）：
/// 取最后一级文件名（大小写不敏感），`frame` + ≥1 位数字 + `.raw` 后缀；
/// 命中即从其所在目录扫描同胞帧（BGRA 直存，零解码）。
pub(crate) fn is_raw_frame_path(path: &str) -> bool {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = base.to_ascii_lowercase();
    let stem = match lower.strip_suffix(".raw") {
        Some(s) => s,
        None => return false,
    };
    let digits = match stem.strip_prefix("frame") {
        Some(d) => d,
        None => return false,
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// 片源格式矩阵（播放器 + 壁纸共用，`Clip` 统一解码）：
/// - `Qois`：QOI 压缩序列（省 U 盘，费解码）；
/// - `Raw`：BGRA 裸帧序列（零解码直搬，费显存/体积）；
/// - `Hevc`：MP4（hev 轨，石雨堂拆包 → Annex-B）或 Annex-B 裸流；
/// - `Avc`：MP4（avc 轨，石雨堂拆 avcC → Annex-B），纯 Rust 软解；
/// - `Vp9`：MP4（vp09 轨，样片即裸帧）或 IVF 容器；
/// - `H263`：裸 H.263 基线流（`SORENSON` 变体不在此列）；
/// - `Mpeg2v`：裸基本流（接入单元切分，只收 4:2:0）；
/// - `Theora`：OggS 磁盘原格式（手写拆包，只收 4:2:0）。
///
/// 非法值整个文件回默认 + 警告（与其它枚举档位同规则，不单独兜底）。
/// 格式不再由配置指定：播放列表/画廊每项按路径自动检测（见 `play::detect_clip_format`），
/// `[play] format` / `[wallpaper] format` 旧键残留按 serde 默认忽略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaFormat {
    Qois,
    Raw,
    Hevc,
    Vp9,
    H263,
    Mpeg2v,
    Theora,
    Avc,
}

fn default_fps() -> String {
    String::from("auto")
}

/// 解码线程档（`[decode.<fmt>] threads`，两档滑杆）：`bsp_only` 安全默认
/// （有状态软解顺序，单核现状）；`all` 在 `qois`/`raw` 立即兑现帧级并行，
/// 有状态软解解析但告警仍 `bsp_only`（每格式安全管线落地前，见 `docs/player.md`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecodeThreads {
    #[default]
    BspOnly,
    All,
}

impl DecodeThreads {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::BspOnly => 0,
            Self::All => 1,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::All,
            _ => Self::BspOnly,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BspOnly => "bsp_only",
            Self::All => "all",
        }
    }
}

pub(crate) const fn default_decode_buffers() -> u32 {
    0
}

/// RAW-GEOM：`[decode.raw] width` 默认 1920（缺键照旧；钳 1..=4096 见 `decode_raw_wh`）。
pub(crate) const fn default_raw_width() -> u32 {
    1920
}

/// RAW-GEOM：`[decode.raw] height` 默认 1080（缺键照旧；钳 1..=4096 见 `decode_raw_wh`）。
pub(crate) const fn default_raw_height() -> u32 {
    1080
}

/// 每格式解码项（`[decode.<fmt>]` 五键 + RAW-GEOM 两键（仅 `raw` 用），见 `DecodeTable`）：
/// - `threads`：`bsp_only`（默认，顺序安全）/`all`（qois/raw 立即兑现，
///   有状态告警仍顺序）；
/// - `zero_copy`：单选开关（默认 false；`feed=zero_copy` 照常走零拷复用，
///   本项为叠加 hint，稳态零分配）；
/// - `streaming`：单选开关（默认 false；`feed=stream` 照常窗口边播边解，
///   `raw` 逐文件 + `h263` reader 恒流式）；
/// - `buffers`：滑杆下标（0 = 跟随 `tune.window` 全局默认，1-32 显式覆盖每格式）；
/// - `fetch`：每格式供片覆盖（默认 None = 继承全局 `[play] feed`；显式值只覆盖本格式，壁纸恒 clip 忽略覆盖）。
/// - `width`/`height`：RAW-GEOM，仅 `[decode.raw]` 生效（默认 1920x1080 = 老行为；
///   其它七表残留键按未知键忽略，不断解析）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecodeEntry {
    #[serde(default)]
    pub threads: DecodeThreads,
    #[serde(default)]
    pub zero_copy: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default = "default_decode_buffers")]
    pub buffers: u32,
    #[serde(default)]
    pub fetch: Option<FeedMode>,
    // RAW-GEOM：raw 帧几何（其余格式忽略本两键）。
    #[serde(default = "default_raw_width")]
    pub width: u32,
    #[serde(default = "default_raw_height")]
    pub height: u32,
}

impl Default for DecodeEntry {
    fn default() -> Self {
        Self {
            threads: DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            // RAW-GEOM：缺表缺键回 1920x1080，老文件逐字节一致。
            width: 1920,
            height: 1080,
        }
    }
}

/// 每格式解码表（`[decode.<fmt>]`，八格式全键，缺表缺键全默认；无别名，未知键忽略）：
/// GUI 解码页只摆六软解（hevc/avc/vp9/h263/mpeg2v/theora 五键）；
/// qois/raw 项保留（`threads=all` 立即兑现，其余全 yes；`fetch` 同式覆盖），供矩阵/串口同口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecodeTable {
    #[serde(default)]
    pub qois: DecodeEntry,
    #[serde(default)]
    pub raw: DecodeEntry,
    #[serde(default)]
    pub hevc: DecodeEntry,
    #[serde(default)]
    pub vp9: DecodeEntry,
    #[serde(default)]
    pub h263: DecodeEntry,
    #[serde(default)]
    pub mpeg2v: DecodeEntry,
    #[serde(default)]
    pub theora: DecodeEntry,
    #[serde(default)]
    pub avc: DecodeEntry,
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

/// 供片模式（`[play] feed`，只管 `Clip` 拿帧的方式，不管上传路）：
/// - `Clip`（默认）：全量预解码（现行行为，老文件缺键照读，零行为变化）；
/// - `Staged`：qois 走显式三段（QOI→Raw→Blt，`buffer.rs` 三缓冲复用）逐帧解码，
///   其余七格式在结构上等价于 `clip`（软解码 planar→BGRA 固有分段，见 `docs/player.md`）；
/// - `Stream`：`decode_buffers(fmt)` 窗口边播边解（0 跟随 `tune.window`，
///   1-32 显式覆盖），游标之后淘汰（`rewind` 重建解码器状态重播）；
/// - `ZeroCopy`：单复用帧缓冲逐显示帧即时解码（调用方须立即上传，
///   `blt/direct_loop` 与 `nano_pump` 满足此借用契约）；
/// - `Single`：零缓存逐帧重解（每帧重读文件 + 重解码 + `render.blt` 整推，
///   与旧 `single_loop` 同语义；`mp_draw`/`dynamic_draw`/壁纸恒 `clip`，见 `feed_for`）。
/// 上传路（blt/direct/mp/nano/dynamic）零改：只消费 `next_frame`/`rewind`/`len`/`frame`。
/// 非法值整个文件回默认 + 警告（与其它枚举档位同规则，不单独兜底）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedMode {
    #[default]
    Clip,
    Staged,
    Stream,
    ZeroCopy,
    Single,
}

impl FeedMode {
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            Self::Clip => 0,
            Self::Staged => 1,
            Self::Stream => 2,
            Self::ZeroCopy => 3,
            Self::Single => 4,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Staged,
            2 => Self::Stream,
            3 => Self::ZeroCopy,
            4 => Self::Single,
            _ => Self::Clip,
        }
    }

    /// 配置文件拼写（serde snake_case 同形，供串口 `video: feed=` 行，
    /// 矩阵断言小写不断言画质）。
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Staged => "staged",
            Self::Stream => "stream",
            Self::ZeroCopy => "zero_copy",
            Self::Single => "single",
        }
    }
}

/// `[play]`：`fps` 是字符串（三态没法用纯数字表达），`measured_max_fps`
/// 只由固件实测回写（0 = 未测），GUI 只读展示。`format` 旧键已删除
/// （片源改走 `[playlist]`，每项按路径自动检测，残留按 serde 默认忽略，无别名；
/// 旧 `[play] pipeline` 同理未知忽略，不再照读）。
/// `feed` 切供片（默认 `clip`，老文件缺键照读；每项有效档 = `decode.<fmt>.fetch`
/// 覆盖 else 全局，见 `decode::effective_feed`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayConfig {
    #[serde(default = "default_fps")]
    pub fps: String,
    #[serde(default)]
    pub measured_max_fps: u32,
    /// 供片模式（默认 clip，老文件缺键照读；只切 `Clip` 内部拿帧方式）。
    #[serde(default)]
    pub feed: FeedMode,
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
