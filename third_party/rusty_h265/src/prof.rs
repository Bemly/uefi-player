//! Stage profiler — where decode time actually goes.
//!
//! Enabled by the `prof` feature and switched on at runtime with `RH265_PROF=1`.
//! Compiled out otherwise: every scope below becomes nothing, so the shipping
//! binary is unaffected and the numbers we publish are still taken from a build
//! with no instrumentation in it at all.
//!
//! # Why this exists
//!
//! Roughly ninety measured wins were landed across the preceding rounds using
//! static instruction counts and guard-branch counts from the emitted assembly.
//! Those are honest counters of *work removed* and they are not a map of *where
//! the time is*: the bottleneck moves after every win, and nothing had looked
//! since the kernels landed.
//!
//! # The profiler is part of the system under test
//!
//! `codec-measurement` §6: a scope guard is a clock read, and at millions of
//! calls it becomes the thing being measured. Two consequences are built in
//! here rather than left to the reader:
//!
//! * **Every stage reports its CALL COUNT next to its time.** A stage entered
//!   2.5 M times is inflated and a stage entered 60 times is not, and you cannot
//!   tell which is which from a percentage.
//! * **The tax is measured, not assumed.** At startup the harness times a large
//!   number of empty scopes to get the per-scope cost on this machine, then
//!   reports `calls × cost` per stage as an explicit deduction. If a stage's tax
//!   is a large fraction of its own total, its number is noise and the report
//!   says so instead of printing a confident percentage.
//!
//! Stages are therefore chosen to be as coarse as still answers the question.
//! `residual_block` runs millions of times a clip and is deliberately NOT a
//! stage of its own: it is inside `parse`, and the way to price it is ablation
//! on an uninstrumented binary.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The stages. Coarse on purpose — see the module note on the profiler's tax.
#[derive(Copy, Clone, Debug)]
#[repr(usize)]
pub enum Stage {
    /// Entropy decode and syntax for one CTU, including everything it calls
    /// that is not separately timed below.
    Parse = 0,
    /// Intra prediction, per transform block.
    Intra,
    /// Inter prediction: motion vector derivation, interpolation, weighting.
    Inter,
    /// Inverse transform and dequant, per transform block.
    Transform,
    /// Deblocking, per picture.
    Deblock,
    /// SAO, per picture.
    Sao,
    /// Reference-picture management, output reordering, everything after the
    /// last sample of a picture is written.
    Dpb,
    /// Motion compensation proper: reference fetch, interpolation, weighting
    /// and the write into the picture. Nests inside `Inter`, whose remainder is
    /// then merge/AMVP derivation and the syntax around it.
    Mc,
    /// The combine step alone: reading the `i16` prediction buffer back and
    /// writing the picture. Nests inside `Mc`. Measured to price fusing it into
    /// the interpolation's final pass.
    Combine,
    /// Building the edge-clamped reference footprint into scratch, for the ~12%
    /// of prediction blocks whose filter window hangs off the picture. Nests
    /// inside `Mc`. Priced to decide whether padding the reference planes once
    /// per picture would be cheaper than copying per block.
    Pad,
    /// Copying a deblocked plane aside so SAO can read unfiltered samples while
    /// writing filtered ones. Nests inside `Sao`.
    SaoCopy,
    /// Allocating and zeroing one picture's sample planes. Nests inside `Dpb`.
    PicAlloc,
}

pub const N: usize = 12;
const NAMES: [&str; N] = ["parse", "intra", "inter", "transform", "deblock", "sao", "dpb", "  |- mc", "  |- combine", "  |- pad", "  |- sao copy", "  |- pic alloc"];
/// Which stages run INSIDE `Parse`. `Parse` wraps a whole CTU, so its raw total
/// includes prediction and transform; reporting all six as if they were
/// siblings sums to 124% and prints a residue of zero, which is what the first
/// run of this profiler did.
const NESTED_IN_PARSE: [Stage; 3] = [Stage::Intra, Stage::Inter, Stage::Transform];
/// `Mc` nests inside `Inter`; it is reported as a breakdown line and must not
/// be added to the column again.
const NESTED_IN_INTER: [Stage; 1] = [Stage::Mc];
/// `Combine` nests inside `Mc`; both are breakdown lines under `Inter` and
/// neither is added to the column again.
const NESTED_IN_MC: [Stage; 2] = [Stage::Combine, Stage::Pad];
/// `SaoCopy` nests inside `Sao`.
const NESTED_IN_SAO: [Stage; 1] = [Stage::SaoCopy];
/// `PicAlloc` nests inside `Dpb`; the remainder of `Dpb` is the per-4x4 maps.
const NESTED_IN_DPB: [Stage; 1] = [Stage::PicAlloc];

static NS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static ON: AtomicBool = AtomicBool::new(false);
/// Measured cost of one empty scope, in nanoseconds ×1000 (fixed point, so the
/// deduction stays honest when a scope costs a fraction of a nanosecond).
static TAX_MNS: AtomicU64 = AtomicU64::new(0);

/// Turn profiling on and measure this machine's per-scope cost.
///
/// Calibration is a real measurement rather than a constant: the cost of a
/// `Instant::now()` pair varies by an order of magnitude across platforms and
/// clock sources, and a wrong constant would silently under- or over-deduct.
pub fn enable() {
    const CAL: u64 = 200_000;
    let t = std::time::Instant::now();
    for _ in 0..CAL {
        let s = Scope::start_raw();
        std::hint::black_box(&s);
        s.stop_raw(Stage::Parse as usize, false);
    }
    let per = t.elapsed().as_nanos() as u64 * 1000 / CAL;
    // The calibration loop itself ran through the accumulators; clear them.
    for i in 0..N {
        NS[i].store(0, Ordering::Relaxed);
        CALLS[i].store(0, Ordering::Relaxed);
    }
    TAX_MNS.store(per, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
}

#[inline(always)]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// A running stage timer. Held by value so the scope ends where the value is
/// dropped, and `#[inline(always)]` so a disabled profiler costs one predicted
/// branch rather than a call.
pub struct Scope {
    t: std::time::Instant,
    stage: usize,
}

impl Scope {
    #[inline(always)]
    fn start_raw() -> Scope {
        Scope { t: std::time::Instant::now(), stage: 0 }
    }

    #[inline(always)]
    fn stop_raw(self, stage: usize, count: bool) {
        NS[stage].fetch_add(self.t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if count {
            CALLS[stage].fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn new(stage: Stage) -> Option<Scope> {
        if enabled() {
            Some(Scope { t: std::time::Instant::now(), stage: stage as usize })
        } else {
            None
        }
    }
}

impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        NS[self.stage].fetch_add(self.t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        CALLS[self.stage].fetch_add(1, Ordering::Relaxed);
    }
}

/// The report: time, share, calls, and the profiler's own tax per stage.
pub fn report(total_ns: u64) -> String {
    let tax_mns = TAX_MNS.load(Ordering::Relaxed);
    let mut out = String::new();
    out.push_str("\nstage        ms        %   calls        ns/call   profiler tax\n");
    out.push_str("---------------------------------------------------------------\n");
    let mut sum = 0u64;
    let mut sum_tax = 0u64;
    let nested: u64 = NESTED_IN_PARSE.iter().map(|s| NS[*s as usize].load(Ordering::Relaxed)).sum();
    let in_inter: u64 = NESTED_IN_INTER.iter().map(|s| NS[*s as usize].load(Ordering::Relaxed)).sum();
    let in_mc: u64 = NESTED_IN_MC.iter().map(|s| NS[*s as usize].load(Ordering::Relaxed)).sum();
    let in_sao: u64 = NESTED_IN_SAO.iter().map(|s| NS[*s as usize].load(Ordering::Relaxed)).sum();
    let in_dpb: u64 = NESTED_IN_DPB.iter().map(|s| NS[*s as usize].load(Ordering::Relaxed)).sum();
    for i in 0..N {
        let raw = NS[i].load(Ordering::Relaxed);
        // `parse` is reported EXCLUSIVE of the stages nested inside it, so the
        // column adds up to the decode and the residue means something.
        let ns = if i == Stage::Parse as usize {
            raw.saturating_sub(nested)
        } else if i == Stage::Inter as usize {
            raw.saturating_sub(in_inter)
        } else if i == Stage::Mc as usize {
            raw.saturating_sub(in_mc)
        } else if i == Stage::Sao as usize {
            raw.saturating_sub(in_sao)
        } else if i == Stage::Dpb as usize {
            raw.saturating_sub(in_dpb)
        } else {
            raw
        };
        let calls = CALLS[i].load(Ordering::Relaxed);
        if calls == 0 {
            continue;
        }
        let tax = calls * tax_mns / 1000;
        sum += ns;
        sum_tax += tax;
        // A stage whose tax is a large slice of its own total is a stage whose
        // number should not be read as a measurement.
        let flag = if ns > 0 && tax * 4 > ns { "  <-- TAX-DOMINATED" } else { "" };
        out.push_str(&format!(
            "{:<9} {:>7.1} {:>7.1}% {:>9} {:>13.1} {:>9.1} ms{}\n",
            NAMES[i],
            ns as f64 / 1e6,
            if total_ns > 0 { 100.0 * ns as f64 / total_ns as f64 } else { 0.0 },
            calls,
            ns as f64 / calls as f64,
            tax as f64 / 1e6,
            flag
        ));
    }
    out.push_str("---------------------------------------------------------------\n");
    let residue = total_ns.saturating_sub(sum);
    out.push_str(&format!(
        "{:<9} {:>7.1} {:>7.1}%   (untimed: everything not in a stage above)\n",
        "residue",
        residue as f64 / 1e6,
        if total_ns > 0 { 100.0 * residue as f64 / total_ns as f64 } else { 0.0 }
    ));
    out.push_str(&format!(
        "{:<9} {:>7.1}          total decode, and {:.1} ms of that is this profiler\n",
        "total",
        total_ns as f64 / 1e6,
        sum_tax as f64 / 1e6
    ));
    out.push_str(&format!(
        "\nper-scope cost measured at {:.2} ns on this machine. Compare the tax\n\
         column against each stage before believing its share (codec-measurement §6):\n\
         if the residue is close to the total tax there is nothing hidden in it.\n",
        tax_mns as f64 / 1000.0
    ));
    out
}
