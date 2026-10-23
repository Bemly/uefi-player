//! 开机待测/显隐规则（`rules` 触达拆分：`rules.rs` 已超 1000 行红线，
//! 纯规则按域下沉，本文件只收 bench 域；行为零改，对外路径经 `rules` 重导出不变）。

use crate::schema::*;

/// 微调区四旋钮显隐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TuneVisible {
    pub(crate) frames: bool,
    pub(crate) tile: bool,
    pub(crate) vfps: bool,
    pub(crate) buf: bool,
}

/// 开机矩阵待测（与固件 `config::needs_render_bench` 同逻辑 + half）：
/// Blt 下 workers/dist 不参测，单核下 dist 不参测。
/// `workers`/`dist` 只认 `[schedule]`（`[render]` 下同名键已删，老文件残留直接忽略）。
pub(crate) fn bench_pending(cfg: &FileConfig) -> bool {
    let (m, s) = (&cfg.render, &cfg.schedule);
    if m.mode == RenderMode::Auto && m.measured_render == RenderMode::Auto {
        return true;
    }
    let render_is_blt = match m.mode {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => m.measured_render == RenderMode::Blt,
    };
    if !render_is_blt && s.workers == WorkersMode::Auto && m.measured_workers == WorkersMode::Auto {
        return true;
    }
    let dist_in_play = !render_is_blt
        && s.workers != WorkersMode::BspOnly
        && !(s.workers == WorkersMode::Auto && m.measured_workers == WorkersMode::BspOnly);
    if dist_in_play && s.dist == DistMode::Auto && m.measured_dist == DistMode::Auto {
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
pub(crate) fn tune_visible(cfg: &FileConfig) -> TuneVisible {
    let (m, s) = (&cfg.render, &cfg.schedule);
    let dist_in_play = m.mode != RenderMode::Blt && s.workers != WorkersMode::BspOnly;
    TuneVisible {
        frames: bench_pending(cfg),
        tile: dist_in_play && (s.dist == DistMode::Auto || s.dist == DistMode::Tile),
        vfps: m.vsync == VsyncMode::Auto || m.vsync == VsyncMode::Static,
        buf: m.mode != RenderMode::Blt,
    }
}
