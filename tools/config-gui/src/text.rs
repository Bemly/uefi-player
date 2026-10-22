//! 开关旁的一句话说明。

use rust_i18n::t;

use crate::app::DecodeTab;
use crate::display::{BarColor, DisplayAlign, DisplayFit};
use crate::schema::*;

pub(crate) fn level_desc(v: AccelLevel) -> String {
    match v {
        AccelLevel::Auto => t!("accel.desc_auto"),
        AccelLevel::Scalar => t!("accel.desc_scalar"),
        AccelLevel::Sse => t!("accel.desc_sse"),
        AccelLevel::Sse2 => t!("accel.desc_sse2"),
        AccelLevel::Sse3 => t!("accel.desc_sse3"),
        AccelLevel::Ssse3 => t!("accel.desc_ssse3"),
        AccelLevel::Sse41 => t!("accel.desc_sse41"),
        AccelLevel::Sse42 => t!("accel.desc_sse42"),
        AccelLevel::Popcnt => t!("accel.desc_popcnt"),
        AccelLevel::Aes => t!("accel.desc_aes"),
        AccelLevel::Pclmulqdq => t!("accel.desc_pclmulqdq"),
        AccelLevel::Avx => t!("accel.desc_avx"),
        AccelLevel::F16c => t!("accel.desc_f16c"),
        AccelLevel::Fma => t!("accel.desc_fma"),
        AccelLevel::Avx2 => t!("accel.desc_avx2"),
        AccelLevel::Bmi1 => t!("accel.desc_bmi1"),
        AccelLevel::Bmi2 => t!("accel.desc_bmi2"),
        AccelLevel::Lzcnt => t!("accel.desc_lzcnt"),
        AccelLevel::Movbe => t!("accel.desc_movbe"),
        AccelLevel::Rdrand => t!("accel.desc_rdrand"),
        AccelLevel::Rdseed => t!("accel.desc_rdseed"),
        AccelLevel::Sha => t!("accel.desc_sha"),
        AccelLevel::Gfni => t!("accel.desc_gfni"),
        AccelLevel::Avx512f => t!("accel.desc_avx512f"),
        AccelLevel::Avx512cd => t!("accel.desc_avx512cd"),
        AccelLevel::Avx512dq => t!("accel.desc_avx512dq"),
        AccelLevel::Avx512bw => t!("accel.desc_avx512bw"),
        AccelLevel::Avx512vl => t!("accel.desc_avx512vl"),
        AccelLevel::Avxvnni => t!("accel.desc_avxvnni"),
        AccelLevel::Tbm => t!("accel.desc_tbm"),
        AccelLevel::Sse4a => t!("accel.desc_sse4a"),
    }
    .into_owned()
}

pub(crate) fn mode_desc(v: RenderMode) -> String {
    match v {
        RenderMode::Auto => t!("accel.desc_mode_auto"),
        RenderMode::Direct => t!("accel.desc_mode_direct"),
        RenderMode::Blt => t!("accel.desc_mode_blt"),
    }
    .into_owned()
}

pub(crate) fn wp_enable_desc(on: bool) -> String {
    if on {
        t!("wallpaper.desc_on")
    } else {
        t!("wallpaper.desc_off")
    }
    .into_owned()
}

pub(crate) fn blt_desc(v: BltOpKind) -> String {
    match v {
        BltOpKind::BufferToVideo => t!("accel.desc_b2v"),
        BltOpKind::VideoToVideo => t!("accel.desc_v2v"),
        BltOpKind::VideoFill => t!("accel.desc_fill"),
        BltOpKind::VideoToBltBuffer => t!("accel.desc_v2b"),
    }
    .into_owned()
}

pub(crate) fn workers_desc(v: WorkersMode) -> String {
    match v {
        WorkersMode::Auto => t!("accel.desc_workers_auto"),
        WorkersMode::BspOnly => t!("accel.desc_workers_bsp"),
        WorkersMode::All => t!("accel.desc_workers_all"),
    }
    .into_owned()
}

pub(crate) fn dist_desc(v: DistMode) -> String {
    match v {
        DistMode::Auto => t!("accel.desc_dist_auto"),
        DistMode::Static => t!("accel.desc_dist_static"),
        DistMode::Tile => t!("accel.desc_dist_tile"),
        DistMode::Dynamic => t!("accel.desc_dist_dynamic"),
    }
    .into_owned()
}

pub(crate) fn async_desc(v: AsyncExecutor) -> String {
    match v {
        AsyncExecutor::All => t!("async.desc_all"),
        AsyncExecutor::Nano => t!("async.desc_nano"),
        AsyncExecutor::Dynamic => t!("async.desc_dynamic"),
    }
    .into_owned()
}

pub(crate) fn sched_desc(v: ScheduleBackend) -> String {
    match v {
        ScheduleBackend::Manual => t!("sched.desc_manual"),
        ScheduleBackend::Nano => t!("sched.desc_nano"),
        ScheduleBackend::Dynamic => t!("sched.desc_dynamic"),
    }
    .into_owned()
}

pub(crate) fn roles_desc(v: RolesMode) -> String {
    match v {
        RolesMode::Auto => t!("sched.desc_roles_auto"),
        RolesMode::Combined => t!("sched.desc_roles_combined"),
        RolesMode::Split => t!("sched.desc_roles_split"),
    }
    .into_owned()
}

/// 核心上限行文本（0 = 自动不限，N>0 = 至多 N 核）。
pub(crate) fn cores_text(cores: u32) -> String {
    if cores == 0 {
        t!("sched.cores_auto").into_owned()
    } else {
        format!("{} cores", cores.min(64))
    }
}

/// 核心上限说明（0/auto 语义 + 取值范围）。
pub(crate) fn cores_desc() -> String {
    t!("sched.desc_cores").into_owned()
}

/// 解码页顶部 tab 短名：`Global` 走 `decode.tab_global`，格式取文件串大写
/// （QOIS/RAW/HEVC…；tab 一行只求可扫读，不用 `format_name` 全称）。
pub(crate) fn decode_tab_name(tab: DecodeTab) -> String {
    match tab {
        DecodeTab::Global => t!("decode.tab_global").into_owned(),
        DecodeTab::Format(fmt) => fmt.file_str().to_uppercase(),
    }
}

pub(crate) fn vsync_desc(v: VsyncMode) -> String {
    match v {
        VsyncMode::Auto => t!("accel.desc_vsync_auto"),
        VsyncMode::Static => t!("accel.desc_vsync_static"),
        VsyncMode::Half => t!("accel.desc_vsync_half"),
        VsyncMode::Off => t!("accel.desc_vsync_off"),
    }
    .into_owned()
}

pub(crate) fn format_name(v: MediaFormat) -> String {
    match v {
        MediaFormat::Qois => t!("format.qois"),
        MediaFormat::Raw => t!("format.raw"),
        MediaFormat::Hevc => t!("format.hevc"),
        MediaFormat::Vp9 => t!("format.vp9"),
        MediaFormat::H263 => t!("format.h263"),
        MediaFormat::Mpeg2v => t!("format.mpeg2v"),
        MediaFormat::Theora => t!("format.theora"),
        MediaFormat::Avc => t!("format.avc"),
    }
    .into_owned()
}

pub(crate) fn format_desc(v: MediaFormat) -> String {
    match v {
        MediaFormat::Qois => t!("format.desc_qois"),
        MediaFormat::Raw => t!("format.desc_raw"),
        MediaFormat::Hevc => t!("format.desc_hevc"),
        MediaFormat::Vp9 => t!("format.desc_vp9"),
        MediaFormat::H263 => t!("format.desc_h263"),
        MediaFormat::Mpeg2v => t!("format.desc_mpeg2v"),
        MediaFormat::Theora => t!("format.desc_theora"),
        MediaFormat::Avc => t!("format.desc_avc"),
    }
    .into_owned()
}

/// 循环模式显示名（存盘仍用 `file_str` 小写串，与固件 `LoopMode` 同构）。
pub(crate) fn loop_desc(v: LoopMode) -> String {
    match v {
        LoopMode::LoopSingle => t!("loop.desc_loop_single"),
        LoopMode::LoopList => t!("loop.desc_loop_list"),
        LoopMode::Once => t!("loop.desc_once"),
    }
    .into_owned()
}

/// 供片模式显示名（存盘仍用 `file_str` 小写串）。
/// 多核（`mp_draw`）/动态调度/壁纸恒为 `clip`（见 `play.feed_hint` 文案）。
pub(crate) fn feed_desc(v: FeedMode) -> String {
    match v {
        FeedMode::Clip => t!("feed.desc_clip"),
        FeedMode::Staged => t!("feed.desc_staged"),
        FeedMode::Stream => t!("feed.desc_stream"),
        FeedMode::ZeroCopy => t!("feed.desc_zero_copy"),
        FeedMode::Single => t!("feed.desc_single"),
    }
    .into_owned()
}

/// 解码线程显示名（存盘仍用 `file_str` 小写串）。
pub(crate) fn decode_threads_desc(v: DecodeThreads) -> String {
    match v {
        DecodeThreads::BspOnly => t!("decode.desc_threads_bsp"),
        DecodeThreads::All => t!("decode.desc_threads_all"),
    }
    .into_owned()
}

/// 逐格式供片覆盖显示名（`None` = 继承全局 `[play] feed`，落盘不写键；
/// `Some` 显示 `file_str: desc`，与播放页供片滑杆同式，存盘仍用 `file_str` 小写串）。
pub(crate) fn decode_fetch_desc(v: Option<FeedMode>) -> String {
    match v {
        None => t!("decode.fetch_inherit").into_owned(),
        Some(f) => format!("{}: {}", f.file_str(), feed_desc(f)),
    }
}

/// 画面适配显示名（存盘仍用 `file_str` 小写串，与固件同构）。
pub(crate) fn display_fit_desc(v: DisplayFit) -> String {
    match v {
        DisplayFit::Stretch => t!("display.desc_fit_stretch"),
        DisplayFit::Fit => t!("display.desc_fit_fit"),
        DisplayFit::Cover => t!("display.desc_fit_cover"),
        DisplayFit::ScaleDown => t!("display.desc_fit_scale_down"),
        DisplayFit::Native => t!("display.desc_fit_native"),
    }
    .into_owned()
}

/// 黑边填充色说明（预设走本地化，自定义走 `#rrggbb (r,g,b)`；存盘一律 hex 小写无 '#'）。
pub(crate) fn display_bar_desc(v: BarColor) -> String {
    if v == BarColor::BLACK {
        t!("display.desc_bar_black")
    } else if v == BarColor::WHITE {
        t!("display.desc_bar_white")
    } else if v == BarColor::GRAY {
        t!("display.desc_bar_gray")
    } else if v == BarColor::NAVY {
        t!("display.desc_bar_navy")
    } else {
        return format!("#{} ({},{},{})", v.to_hex(), v.r, v.g, v.b);
    }
    .into_owned()
}

/// 画面对齐显示名（存盘仍用小写串；stretch 忽略对齐）。
pub(crate) fn display_align_desc(v: DisplayAlign) -> String {
    match v {
        DisplayAlign::Center => t!("display.desc_align_center"),
        DisplayAlign::Left => t!("display.desc_align_left"),
        DisplayAlign::Right => t!("display.desc_align_right"),
        DisplayAlign::Top => t!("display.desc_align_top"),
        DisplayAlign::Bottom => t!("display.desc_align_bottom"),
    }
    .into_owned()
}

/// 转码目标显示名（输出名见 `TranscodeTarget::out_path`，与固件片源名一致）。
pub(crate) fn transcode_target_name(v: crate::transcode::TranscodeTarget) -> String {
    use crate::transcode::TranscodeTarget;
    match v {
        TranscodeTarget::Avc => t!("transcode.t_avc"),
        TranscodeTarget::Hevc => t!("transcode.t_hevc"),
        TranscodeTarget::Vp9 => t!("transcode.t_vp9"),
        TranscodeTarget::H263 => t!("transcode.t_h263"),
        TranscodeTarget::Mpeg2v => t!("transcode.t_mpeg2v"),
        TranscodeTarget::Theora => t!("transcode.t_theora"),
        TranscodeTarget::Qois => t!("transcode.t_qois"),
        TranscodeTarget::Raw => t!("transcode.t_raw"),
    }
    .into_owned()
}

/// 转码目标一句话说明（编码器 + 容器去向）。
pub(crate) fn transcode_target_desc(v: crate::transcode::TranscodeTarget) -> String {
    use crate::transcode::TranscodeTarget;
    match v {
        TranscodeTarget::Avc => t!("transcode.d_avc"),
        TranscodeTarget::Hevc => t!("transcode.d_hevc"),
        TranscodeTarget::Vp9 => t!("transcode.d_vp9"),
        TranscodeTarget::H263 => t!("transcode.d_h263"),
        TranscodeTarget::Mpeg2v => t!("transcode.d_mpeg2v"),
        TranscodeTarget::Theora => t!("transcode.d_theora"),
        TranscodeTarget::Qois => t!("transcode.d_qois"),
        TranscodeTarget::Raw => t!("transcode.d_raw"),
    }
    .into_owned()
}
