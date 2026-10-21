//! 开关旁的一句话说明。

use rust_i18n::t;

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

pub(crate) fn vsync_desc(v: VsyncMode) -> String {
    match v {
        VsyncMode::Auto => t!("accel.desc_vsync_auto"),
        VsyncMode::Static => t!("accel.desc_vsync_static"),
        VsyncMode::Half => t!("accel.desc_vsync_half"),
        VsyncMode::Off => t!("accel.desc_vsync_off"),
    }
    .into_owned()
}
