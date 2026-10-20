//! Per-stage wall-clock accounting for `examples/bench.rs`'s `--stages` breakdown
//! (the measurement infrastructure summarized under M5 in `docs/history.md`).
//!
//! Call sites (`src/lib.rs::decode_one_frame`, `src/tile/residual.rs`) hold a
//! [`StageTimer`] for the duration of a stage; on drop it adds the elapsed time to a
//! thread-local per-[`Stage`] counter. `reset`/`snapshot` let the caller (the bench
//! example) zero the counters before a decode run and read them back after. Tile workers
//! return their thread-local snapshots to the parent thread, which merges them before the
//! benchmark reads the final snapshot.
//!
//! Zero-cost when the `bench-timing` feature is off: the `imp` module below is swapped
//! for a stub whose `StageTimer` is a field-less struct with no `Drop` impl and an
//! `#[inline(always)]` no-op constructor, so `StageTimer::start(..)` calls at every call
//! site optimize away entirely (no `Instant::now()`, no counter write). `Stage` itself
//! stays unconditional so call sites don't need `#[cfg]` at all.

/// One of the coarse per-frame stages timed in `decode_one_frame`, plus the finer-grained
/// stages timed inside the tile-decode residual path (`src/tile/residual.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// The whole of `decode_one_frame` (denominator for the coarse breakdown; the other
    /// coarse stages should sum close to this -- see `--stages`' "other/gap" row).
    Total,
    HeaderParse,
    CompressedHeader,
    TileDecode,
    LoopFilter,
    DpbOutput,
    /// Inside `TileDecode`: `tokens_and_reconstruct` (token decode + dequant + inverse
    /// transform + reconstruction).
    TokenDequantTransform,
    /// Inside `TileDecode`: `predict_inter` (motion compensation / subpel convolution).
    InterPredict,
    /// Inside `TileDecode`: `predict_intra` (`predict_intra_block`).
    IntraPredict,
    /// Inside `TokenDequantTransform` (a SUBSET of it, not additive): just the
    /// `inverse_transform_block` call -- isolates the inverse-transform cost from token
    /// decode + dequant + reconstruction, to size a potential transform SIMD wave.
    InverseTransform,
}

/// Number of [`Stage`] variants (= array size for [`snapshot`]), derived from the last
/// variant so it can't drift from the enum.
pub const STAGE_COUNT: usize = Stage::InverseTransform as usize + 1;

#[cfg(feature = "bench-timing")]
mod imp {
    use super::Stage;
    use std::cell::Cell;
    use std::time::Instant;

    thread_local! {
        static COUNTERS: [Cell<u64>; super::STAGE_COUNT] = const {
            [
                Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0),
                Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0),
            ]
        };
    }

    fn add(stage: Stage, nanos: u64) {
        COUNTERS.with(|c| {
            let cell = &c[stage as usize];
            cell.set(cell.get() + nanos);
        });
    }

    /// Zeroes all per-stage counters (call before timing a fresh decode run).
    pub fn reset() {
        COUNTERS.with(|c| {
            for cell in c {
                cell.set(0);
            }
        });
    }

    /// Reads the accumulated nanoseconds per [`Stage`] (indexed by `Stage as usize`).
    pub fn snapshot() -> [u64; super::STAGE_COUNT] {
        COUNTERS.with(|c| std::array::from_fn(|i| c[i].get()))
    }

    /// Adds a worker thread's snapshot to the current thread's counters.
    // VENDOR(uefi-player): tile workers are std-only; without `std` there is
    // no caller, so this is compiled out.
    #[cfg(feature = "std")]
    pub(crate) fn merge_snapshot(snapshot: &[u64; super::STAGE_COUNT]) {
        COUNTERS.with(|c| {
            for (cell, &nanos) in c.iter().zip(snapshot) {
                cell.set(cell.get().saturating_add(nanos));
            }
        });
    }

    /// RAII stage timer: [`StageTimer::start`] records the start instant; dropping it
    /// (end of the enclosing scope) adds the elapsed time to that stage's counter.
    pub struct StageTimer {
        start: Instant,
        stage: Stage,
    }

    impl StageTimer {
        #[inline]
        pub fn start(stage: Stage) -> Self {
            Self {
                start: Instant::now(),
                stage,
            }
        }
    }

    impl Drop for StageTimer {
        #[inline]
        fn drop(&mut self) {
            add(self.stage, self.start.elapsed().as_nanos() as u64);
        }
    }
}

#[cfg(not(feature = "bench-timing"))]
mod imp {
    use super::Stage;

    pub fn reset() {}

    pub fn snapshot() -> [u64; super::STAGE_COUNT] {
        [0; super::STAGE_COUNT]
    }

    #[inline(always)]
    // VENDOR(uefi-player): tile workers are std-only (see above).
    #[cfg(feature = "std")]
    pub(crate) fn merge_snapshot(_snapshot: &[u64; super::STAGE_COUNT]) {}

    /// Field-less stand-in: `start` is a no-op and there is no `Drop` impl, so this
    /// compiles away completely (see module docs).
    pub struct StageTimer;

    impl StageTimer {
        #[inline(always)]
        pub fn start(_stage: Stage) -> Self {
            Self
        }
    }
}

// VENDOR(uefi-player): tile workers are std-only (see above).
#[cfg(feature = "std")]
pub(crate) use imp::merge_snapshot;
pub use imp::{reset, snapshot, StageTimer};

#[cfg(all(test, feature = "bench-timing"))]
#[path = "../tests/unit/bench_timing.rs"]
mod tests;
