//! `[schedule] cores/roles` 核拓扑引擎（`src/config` 的 crew 半；schema 见
//! `types::ScheduleConfig`，生效值原子存储见本文件）。
//!
//! # BSP-id 假设（启动 subtlety，调用方不再各自假设）
//! UEFI MP Services 下 BSP 的 `who_am_i` 恒为 0（EDK2/mu_msvm 惯例，
//! QEMU/真机实测一致）。crew = `{BSP} ∪ {最低编号 N-1 个 AP}`，即 id
//! `0..k`（`k` = 选中核数），天然含 BSP，无需显式 pin。
//! 若实测 `who_am_i(BSP) != 0`（映射惊喜）：`resolve_crew_count` 记 warn +
//! 回落全核（不断播），调用方（`mp`/`mp_slots`/`exec` 三处分发）统一走它。
//!
//! # 启动方式（`exec::dynamic_draw` 同式，三处统一）
//! start-all-but-idle-unchosen：单次 `startup_all_aps` broadcast，编号 ≥ k
//! 的 AP 进任务函数即 return（`ap_index_in_crew` 判），永不碰 GOP/前台。
//! 不用逐核 `startup_this_ap` 循环：后者把 SIPI 串行化 N 次 + 任一 AP
//! 失败即缺核（还要逐个兜底），broadcast 一次唤醒、闲核早退更稳（闲核
//! 开销 = 一次 `who_am_i` + return，固件实测可忽略）。
//!
//! # 生效集优先级（`effective_crew_count`，调用方只传 detected）
//! `bsp_only` → `{BSP}`；`cores > 0` → 前 `min(cores, detected)`；
//! `measured_workers_n > 0` → 前 `min(measured, detected)`；
//! `workers == all` → 全核；否则（auto 首启未测）校准 sweep 后回写，
//! 播放期按全核兜底（`needs_render_bench` 为真时必先校准，到不了此分支）。
//!
//! # 角色门（`effective_roles`）
//! `combined` 恒行路；`split` 仅 `slots_eligible`（qois/raw + 零拷系供片 +
//! 选中核数 > 1）时槽路，否则 warn + 回落行路；`auto` = 准入即分（现状行为）。

use core::sync::atomic::{AtomicU32, Ordering};

use crate::error::NyaStatus;
use crate::fs::Fs;

use super::{Config, FeedMode, MediaFormat, RolesMode, WorkersMode};

/// 核数记忆（0 = 未测；首启 auto sweep 后回写）。
static MEASURED_N: AtomicU32 = AtomicU32::new(0);

/// 开机 `load()` 唯一调用：P2 起 cores/roles 进 `runtime`（见 `runtime::apply_cfg`），
/// 此处只存核数记忆（钳 64）。
pub(crate) fn store_measured(measured_workers_n: u32) {
    MEASURED_N.store(measured_workers_n.min(64), Ordering::Relaxed);
}

/// 用核预算（0 = auto 不限；N > 0 = 至多 N 核，已钳 64；P2 起读 runtime）。
pub fn schedule_cores() -> u32 {
    super::runtime().schedule_cores.min(64)
}

/// 角色分工（默认 `auto`；P2 起读 runtime）。
pub fn schedule_roles() -> RolesMode {
    super::runtime().schedule_roles
}

/// 核数记忆（0 = 未测）。
pub fn measured_workers_n() -> u32 {
    MEASURED_N.load(Ordering::Relaxed).min(64)
}

pub(crate) fn set_measured_workers_n(n: u32) {
    MEASURED_N.store(n.min(64), Ordering::Relaxed);
}

/// 供片格（qois/raw + 零拷系供片；不含核数，供 `need_eager_clip` 与槽门共用）。
/// 口径与旧 `mp_slots::slots_feed_eligible` 一致（本函数即其新家，旧名删）。
pub fn slots_feed_eligible() -> bool {
    let fmt = super::play_format();
    matches!(fmt, MediaFormat::Qois | MediaFormat::Raw)
        && matches!(
            super::effective_feed(fmt),
            FeedMode::ZeroCopy | FeedMode::Single | FeedMode::Stream
        )
}

/// 槽路准入（含核数）：供片格 + 选中核数 > 1（`workers` 已折进选中集，不另查）。
pub fn slots_eligible(crew_n: usize) -> bool {
    crew_n > 1 && slots_feed_eligible()
}

/// 槽路意向（无核数，供 `need_eager_clip` 乐观预判；分发期以 `slots_eligible`
/// 复核 + `ensure_eager` 兜底，语义与旧版一致）：显式合/单核预算直接否。
pub fn mp_slots_wanted() -> bool {
    super::workers_mode() != WorkersMode::BspOnly
        && schedule_cores() != 1
        && schedule_roles() != RolesMode::Combined
        && slots_feed_eligible()
}

/// 生效角色：`combined` 恒合；`split` 仅准入时分（否则 warn + 合）；
/// `auto` = 准入即分（现状行为），不准入静默合。
pub fn effective_roles(crew_n: usize) -> RolesMode {
    let eligible = slots_eligible(crew_n);
    match schedule_roles() {
        RolesMode::Combined => RolesMode::Combined,
        RolesMode::Split => {
            if eligible {
                RolesMode::Split
            } else {
                log::warn!(
                    "sched: roles=split not eligible (need qois/raw + zero_copy/single/stream + crew>1), fallback combined"
                );
                RolesMode::Combined
            }
        }
        RolesMode::Auto => {
            if eligible {
                RolesMode::Split
            } else {
                RolesMode::Combined
            }
        }
    }
}

/// 生效 workers 三态小写（供 `sched:` 行；`auto` 只在未决议时出现，兜底用）。
pub fn effective_workers_str() -> &'static str {
    match super::effective_workers() {
        WorkersMode::BspOnly => "bsp_only",
        WorkersMode::All => "all",
        WorkersMode::Auto => "auto",
    }
}

/// 分发行（manual `mp_draw` + dynamic 两路统一格式；旧行逐字保留，新增本行）：
/// `sched: crew=<n>/<m> roles=<r> workers=<w>`（n = 选中，m = 检出，
/// r/w = 生效值小写）。
pub fn emit_sched_line(crew_n: usize, detected: usize, roles: RolesMode) {
    log::info!(
        "sched: crew={}/{} roles={} workers={}",
        crew_n,
        detected,
        roles.file_str(),
        effective_workers_str(),
    );
}

/// 生效核数（k）：`{0..k}` 即选中集（含 BSP，见模块头假设）。
pub fn effective_crew_count(detected: usize) -> usize {
    let detected = detected.max(1);
    if super::workers_mode() == WorkersMode::BspOnly {
        return 1;
    }
    let budget = schedule_cores() as usize;
    if budget > 0 {
        return budget.min(detected).max(1);
    }
    let mem = measured_workers_n() as usize;
    if mem > 0 {
        return mem.min(detected).max(1);
    }
    if super::workers_mode() == WorkersMode::All {
        return detected;
    }
    // auto 首启未测：校准 sweep 必先跑并回写（`needs_render_bench` 门控），
    // 到此说明校准被跳过（如 Blt 定死），按全核兜底（旧 `effective_workers` 同值）。
    detected
}

/// 分发期决议（含 BSP-id 自检）：BSP 非 0 即映射惊喜 → warn + 全核回落。
pub fn resolve_crew_count(detected: usize, bsp_id: usize) -> usize {
    let detected = detected.max(1);
    if bsp_id != 0 {
        log::warn!(
            "sched: BSP who_am_i={} != 0 (crew assumes BSP=0), fallback all {} cores",
            bsp_id,
            detected
        );
        return detected;
    }
    effective_crew_count(detected)
}

/// 选中集映射：`who_am_i` →  crew 内下标（`0..k` 恒等；`None` = 闲核，早退）。
/// 三处任务入口（`play_task`/`slot_task`/`exec_ap`）统一用它判。
pub fn ap_index_in_crew(who_am_i: usize, crew_n: usize) -> Option<usize> {
    if who_am_i < crew_n.max(1) {
        Some(who_am_i)
    } else {
        None
    }
}

/// 核数 cells（首启 auto sweep 用，有序去重，至多 3 格；预算封顶）：
/// `{1, half, full}`，`full = min(预算 || 检出, 检出)`，`half = full/2`（≥1）。
pub fn core_cells(detected: usize, budget: u32) -> ([usize; 3], usize) {
    let detected = detected.max(1);
    let full = if budget > 0 {
        (budget as usize).min(detected)
    } else {
        detected
    }
    .max(1);
    let half = (full / 2).max(1);
    let mut cells = [1usize, half, full];
    cells.sort_unstable();
    let mut n = 0usize;
    for i in 0..3 {
        if i == 0 || cells[i] != cells[i - 1] {
            cells[n] = cells[i];
            n += 1;
        }
    }
    (cells, n)
}

/// 5% 裁决（抗噪）：`ticks` 为 `(核数, 累计 ticks)`（同轮次可比）；
/// 最快格比 1 核基线快 ≥ 5% 即取最快（并列取少核），否则取 1 核。
/// 无基线（1 核格缺失）即取最快，不断 calibrating。
pub fn pick_core_count(ticks: &[(usize, u64)]) -> usize {
    if ticks.is_empty() {
        return 1;
    }
    let mut best = ticks[0];
    for &t in &ticks[1..] {
        if t.1 < best.1 || (t.1 == best.1 && t.0 < best.0) {
            best = t;
        }
    }
    let base = ticks
        .iter()
        .filter(|t| t.0 == 1)
        .map(|t| t.1)
        .min()
        .unwrap_or(0);
    if best.0 == 1 || base == 0 {
        return best.0;
    }
    // fastest 比基线快 ≥ 5% ⟺ fastest*20 ≤ base*19（u128 防溢出）。
    if (best.1 as u128).saturating_mul(20) <= (base as u128).saturating_mul(19) {
        best.0
    } else {
        1
    }
}

/// 核数记忆回写 `\player.toml`（`[schedule] measured_workers_n`；`[render]`
/// 三态由 `persist_render_measured` 另行写回做展示兼容，两次重读-改写同策略）。
/// 同时装进运行内存（下次启动复用快启）。
pub fn persist_measured_workers_n(n: u32) -> crate::error::Result {
    let n = n.min(64);
    set_measured_workers_n(n);
    let mut fs = Fs::new()?;
    let mut cfg = match fs.read_file(uefi::cstr16!("player.toml")) {
        Ok(bytes) => toml::from_str::<Config>(&alloc::string::String::from_utf8_lossy(&bytes))
            .unwrap_or_default(),
        Err(_) => Config::default(),
    };
    cfg.schedule.measured_workers_n = n;
    let text = toml::to_string(&cfg)
        .map_err(|_| NyaStatus::_Debug(alloc::string::String::from("cfg: serialize failed")))?;
    fs.write_file(uefi::cstr16!("player.toml"), text.as_bytes())?;
    Ok(())
}
