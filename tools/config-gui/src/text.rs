//! 开关旁的一句话说明。

use rust_i18n::t;

use crate::schema::*;

pub(crate) fn level_desc(v: AccelLevel) -> String {
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
