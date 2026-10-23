//! ptui 状态层：`config::PlayerRuntime` 的 TUI 侧访问封装。
//!
//! - 本文件只做三件事：子页导航类型（`Next`）、文件串小写表（串口用）、
//!   屏显+串口 helpers（`goto`/`set`）。运行时读写直调契约 API：
//!   读经 `config::runtime()`，写经 `config::runtime_mut()`，
//!   壁纸经 `config::set_wallpaper_*`（只改内存）+ `config::save_wallpaper()`
//!   （落盘，只写 `[wallpaper]` 表），媒体目录经 `config::media_dir()`。
//! - 调档只改 runtime 内存，绝不落盘（破坏式要求）；壁纸页显式保存除外。
//! - 屏显统一走 `Screen::draw_str`，串口同步走 `log::`：
//!   每页切换记 `ptui: page=<name>`，每次调档记 `ptui: set <key>=<value>`
//!   （键名用配置文件串小写形，如 `accel.level=sse42`，矩阵断言用）。

use crate::config::{
    AccelLevel, BltOpKind, DecodeThreads, DisplayAlign, DisplayFit, DistMode, FeedMode, RenderMode,
    RolesMode, ScheduleBackend, VsyncMode, WorkersMode,
};
use crate::graphics::Screen;

/// 子页导航（各页 `run(screen: Screen) -> Screen` 值进值出，自带按键循环，
/// `q` 返回调用方；首页直接嵌套调子页，外壳只需调首页一处）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Home,
    Play,
    Accel,
    Render,
    Schedule,
    Decode,
    Display,
    Tests,
    Wallpaper,
}

/// `Next` 的串口名（`ptui: page=<name>` 用）。
pub fn page_name(n: Next) -> &'static str {
    match n {
        Next::Home => "home",
        Next::Play => "play",
        Next::Accel => "accel",
        Next::Render => "render",
        Next::Schedule => "schedule",
        Next::Decode => "decode",
        Next::Display => "display",
        Next::Tests => "tests",
        Next::Wallpaper => "wallpaper",
    }
}

// ---------- 文件串小写表（串口 `ptui: set` 与落盘同形） ----------

/// `AccelLevel::as_u8` 下标即档位（`auto` 恒首，共 31 档，见 `config::types`）。
pub const ACCEL_STRS: [&str; 31] = [
    "auto",
    "scalar",
    "sse",
    "sse2",
    "sse3",
    "ssse3",
    "sse41",
    "sse42",
    "popcnt",
    "lzcnt",
    "movbe",
    "aes",
    "pclmulqdq",
    "rdrand",
    "rdseed",
    "sha",
    "gfni",
    "tbm",
    "sse4a",
    "avx",
    "f16c",
    "fma",
    "avx2",
    "bmi1",
    "bmi2",
    "avxvnni",
    "avx512f",
    "avx512cd",
    "avx512dq",
    "avx512bw",
    "avx512vl",
];
pub const ACCEL_COUNT: u8 = 31;
/// `RenderMode::as_u8` 序：Auto=0，Direct=1，Blt=2。
pub const RENDER_STRS: [&str; 3] = ["auto", "direct", "blt"];
/// `VsyncMode::as_u8` 序：Auto=0，Static=1，Off=2，Half=3。
pub const VSYNC_STRS: [&str; 4] = ["auto", "static", "off", "half"];
/// `BltOpKind` 声明序（`blt_to_u8` 同序）：B2V=0，V2V=1，Fill=2，V2B=3。
pub const BLT_STRS: [&str; 4] = [
    "buffer_to_video",
    "video_to_video",
    "video_fill",
    "video_to_blt_buffer",
];
pub const BACKEND_STRS: [&str; 3] = ["manual", "nano", "dynamic"];
pub const WORKERS_STRS: [&str; 3] = ["auto", "bsp_only", "all"];
pub const DIST_STRS: [&str; 4] = ["auto", "static", "tile", "dynamic"];
pub const ROLES_STRS: [&str; 3] = ["auto", "combined", "split"];
pub const FEED_STRS: [&str; 5] = ["clip", "staged", "stream", "zero_copy", "single"];
/// 解码八格式键（`decode.<fmt>.*` 用）。
pub const FMT_KEYS: [&str; 8] = [
    "qois", "raw", "hevc", "vp9", "h263", "mpeg2v", "theora", "avc",
];
/// 供片覆盖串：`inherit`（盘上缺键）+ 五档 feed。
pub const FETCH_STRS: [&str; 6] = ["inherit", "clip", "staged", "stream", "zero_copy", "single"];
/// fps 三态轮换序列（播放/壁纸页 `f` 键用；数字档按常用帧率取）。
pub const FPS_SEQ: [&str; 6] = ["auto", "max", "15", "30", "60", "120"];

/// 布尔转文件串（TOML 小写）。
pub fn onoff(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

/// `BltOpKind`→0-3（`BLT_STRS` 同序；`BltOpKind` 无 `as_u8`，此处自定）。
pub fn blt_to_u8(b: BltOpKind) -> u8 {
    match b {
        BltOpKind::BufferToVideo => 0,
        BltOpKind::VideoToVideo => 1,
        BltOpKind::VideoFill => 2,
        BltOpKind::VideoToBltBuffer => 3,
    }
}

pub fn u8_to_blt(v: u8) -> BltOpKind {
    match v {
        1 => BltOpKind::VideoToVideo,
        2 => BltOpKind::VideoFill,
        3 => BltOpKind::VideoToBltBuffer,
        _ => BltOpKind::BufferToVideo,
    }
}

/// `DecodeThreads`→文件串（`as_str` 已是小写同形）。
pub fn threads_str(t: DecodeThreads) -> &'static str {
    t.as_str()
}

// ---------- 屏显 + 串口 ----------

/// 进页：串口记 `ptui: page=<name>`，屏显清屏复位（调用方接着画标题）。
pub fn goto(screen: &mut Screen, name: &str) {
    log::info!("ptui: page={}", name);
    let _ = screen.clear();
    screen.reset_cursor();
}

/// 调档：串口屏显双写 `ptui: set <key>=<value>`（矩阵断言用）。
pub fn set(screen: &mut Screen, key: &str, val: &str) {
    crate::emit!(screen, "ptui: set {}={}", key, val);
}

// ---------- 读写小 helpers（各页共用，口径与 `runtime::apply_cfg` 一致） ----------

/// accel 档位步进（±1，0-30 回绕）。
pub fn step_accel(cur: AccelLevel, delta: i8) -> AccelLevel {
    let n = cur.as_u8() as i8;
    let next = (n + delta).rem_euclid(ACCEL_COUNT as i8) as u8;
    AccelLevel::from_u8(next)
}

/// 通用回绕步进（`n` 为档数）。
pub fn step_u8(cur: u8, n: u8, delta: i8) -> u8 {
    ((cur as i8 + delta).rem_euclid(n as i8)) as u8
}

/// render mode 回绕（auto/direct/blt）。
pub fn step_render(cur: RenderMode, delta: i8) -> RenderMode {
    RenderMode::from_u8(step_u8(cur.as_u8(), 3, delta))
}

/// vsync 回绕（auto/static/off/half，注意 `as_u8` 序 off=2/half=3）。
pub fn step_vsync(cur: VsyncMode, delta: i8) -> VsyncMode {
    VsyncMode::from_u8(step_u8(cur.as_u8(), 4, delta))
}

/// backend 回绕（manual/nano/dynamic）。
pub fn step_backend(cur: ScheduleBackend, delta: i8) -> ScheduleBackend {
    ScheduleBackend::from_u8(step_u8(cur.as_u8(), 3, delta))
}

/// workers 回绕（auto/bsp_only/all）。
pub fn step_workers(cur: WorkersMode, delta: i8) -> WorkersMode {
    WorkersMode::from_u8(step_u8(cur.as_u8(), 3, delta))
}

/// dist 回绕（auto/static/tile/dynamic）。
pub fn step_dist(cur: DistMode, delta: i8) -> DistMode {
    DistMode::from_u8(step_u8(cur.as_u8(), 4, delta))
}

/// roles 回绕（auto/combined/split）。
pub fn step_roles(cur: RolesMode, delta: i8) -> RolesMode {
    RolesMode::from_u8(step_u8(cur.as_u8(), 3, delta))
}

/// feed 回绕（clip/staged/stream/zero_copy/single）。
pub fn step_feed(cur: FeedMode, delta: i8) -> FeedMode {
    FeedMode::from_u8(step_u8(cur.as_u8(), 5, delta))
}

/// fit 回绕（stretch/fit/cover/scale_down/native）。
pub fn step_fit(cur: DisplayFit, delta: i8) -> DisplayFit {
    DisplayFit::from_u8(step_u8(cur.as_u8(), 5, delta))
}

/// align 回绕（center/left/right/top/bottom）。
pub fn step_align(cur: DisplayAlign, delta: i8) -> DisplayAlign {
    DisplayAlign::from_u8(step_u8(cur.as_u8(), 5, delta))
}

/// threads 两档拨（bsp_only/all）。
pub fn step_threads(cur: DecodeThreads) -> DecodeThreads {
    match cur {
        DecodeThreads::BspOnly => DecodeThreads::All,
        DecodeThreads::All => DecodeThreads::BspOnly,
    }
}
