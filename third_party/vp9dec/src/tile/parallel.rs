//! Tile-parallel decode machinery (the `tile_cols > 1 && tile_rows == 1` fast path of
//! [`TileDecoder::decode_tiles`]): the per-tile-column worker `TileDecoder`s (column-strip
//! buffers), the strip merge back into the frame decoder, the tile-data splitter, and the
//! `std::thread::scope` driver. Split out of `tile.rs` (which keeps the sequential
//! tile/superblock traversal this path is bit-identical to).

// VENDOR(uefi-player): worker threads need `std`; without it the column-strip
// workers and the `thread::scope` driver below are compiled out (only the
// tile-data splitter and the worker-count stub remain, serving the sequential
// loop in `super::TileDecoder::decode_tiles`).
#[cfg(feature = "std")]
use crate::counts::Counts;
#[cfg(feature = "std")]
use crate::framebuffer::Plane;

use alloc::vec::Vec;

use super::{TileDecoder, TileError};
#[cfg(feature = "std")]
use super::{get_tile_offset, MiGrid};

/// Test-only knob: forces [`TileDecoder::decode_tiles`] down the sequential loop even when the
/// tile-parallel fast path would engage, so tests can assert that the parallel and sequential
/// decodes of a multi-tile stream are byte-identical (see `tests/tile_parallel_test.rs`).
/// Compiled only for test builds (the self-referential `test-support` dev-dependency).
#[cfg(feature = "test-support")]
pub static FORCE_SEQUENTIAL_TILES: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Test/benchmark-only override for the worker count reported to tile dispatch. Zero keeps the
/// platform's [`std::thread::available_parallelism`] result; a positive value makes the
/// single-worker fallback and bounded parallel cases reproducible on any development machine.
#[cfg(feature = "test-support")]
pub static FORCE_TILE_WORKERS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

pub(super) fn available_tile_workers() -> usize {
    #[cfg(feature = "test-support")]
    {
        // VENDOR(uefi-player): core path works in both std and no_std builds.
        let forced = FORCE_TILE_WORKERS.load(core::sync::atomic::Ordering::Relaxed);
        if forced > 0 {
            return forced;
        }
    }
    // VENDOR(uefi-player): without `std` there is no thread pool to query; 1
    // funnels the caller into the sequential loop.
    #[cfg(feature = "std")]
    {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1)
    }
    #[cfg(not(feature = "std"))]
    {
        1
    }
}

impl TileDecoder {
    /// Builds a fresh worker `TileDecoder` that shares this frame's read-only / config state
    /// (cheap `Arc` / `Copy` clones) but gets its own zeroed mutable buffers, sized to the tile
    /// column `[mi_col_start, mi_col_end)` it will decode: the planes and `mi_grid` are *column
    /// strips* (see [`Plane::new_strip`] / [`MiGrid::new_strip`] -- indexed by absolute
    /// coordinates, so the decode path is unchanged), not full frames. Used by the tile-parallel
    /// path in [`Self::decode_tiles`]: each tile column decodes on one of these (independent per
    /// VP9's tile-column rule), then its column strip is merged back.
    ///
    /// Strip extents: every access a tile-column decode makes to the current frame stays within
    /// the tile's own columns -- blocks never cross a tile boundary (tile offsets are
    /// superblock-aligned), the left neighbor is gated at the tile edge (`avail_l`), intra
    /// above-right reads are bounded by the prediction block (`not_on_right`), and MV candidate
    /// reads by `is_inside` (tile-bounded) -- EXCEPT that edge superblocks of the *last* tile
    /// column write past `mi_cols`/`MiCols*8` into the superblock-rounded padding (see
    /// `framebuffer.rs`'s module doc). So the last column's strip extends to the full padded
    /// width; the merge never copies that padding (same drop as before).
    ///
    /// The `above_*` context arrays stay full-width (a few bytes per MI column): they are
    /// indexed absolutely, and slicing them would complicate the code for no measurable gain.
    // VENDOR(uefi-player): column-strip worker, std-only (see the imports above).
    #[cfg(feature = "std")]
    pub(super) fn spawn_column_worker(&self, mi_col_start: u32, mi_col_end: u32) -> TileDecoder {
        let grid_cols = self.mi_grid.cols();
        let grid_rows = self.planes[0].height / 8;
        let above_nz_len = grid_cols * 2;
        let last = mi_col_end == self.mi_cols;
        let grid_col_end = if last { grid_cols } else { mi_col_end as usize };
        let strip_plane = |p: usize| {
            let sub = if p == 0 { 0 } else { self.subsampling_x };
            let x0 = (mi_col_start as usize * 8) >> sub;
            let x_end = if last {
                self.planes[p].width
            } else {
                (mi_col_end as usize * 8) >> sub
            };
            Plane::new_strip(x_end - x0, self.planes[p].height, x0)
        };
        TileDecoder {
            tx_mode: self.tx_mode,
            probs: self.probs.clone(),
            segmentation: self.segmentation,
            mi_cols: self.mi_cols,
            mi_rows: self.mi_rows,
            tile_cols_log2: self.tile_cols_log2,
            tile_rows_log2: self.tile_rows_log2,
            mi_grid: MiGrid::new_strip(
                grid_col_end - mi_col_start as usize,
                grid_rows,
                mi_col_start,
            ),
            above_partition_context: vec![0u8; grid_cols],
            left_partition_context: [0u8; 8],
            above_seg_pred_context: vec![0u8; grid_cols],
            left_seg_pred_context: [0u8; 8],
            prev_segment_ids: self.prev_segment_ids.clone(),
            mi_col_start,
            mi_col_end,
            mi_row_start: 0,
            mi_row_end: self.mi_rows,
            bit_depth: self.bit_depth,
            subsampling_x: self.subsampling_x,
            subsampling_y: self.subsampling_y,
            lossless: self.lossless,
            dequant_table: self.dequant_table,
            planes: [strip_plane(0), strip_plane(1), strip_plane(2)],
            above_nonzero_context: [
                vec![0u8; above_nz_len],
                vec![0u8; above_nz_len],
                vec![0u8; above_nz_len],
            ],
            left_nonzero_context: [[0u8; 16]; 3],
            frame_is_intra: self.frame_is_intra,
            ref_frame_sign_bias: self.ref_frame_sign_bias,
            allow_high_precision_mv: self.allow_high_precision_mv,
            interpolation_filter: self.interpolation_filter,
            reference_mode: self.reference_mode,
            comp_fixed_ref: self.comp_fixed_ref,
            comp_var_ref: self.comp_var_ref,
            use_prev_frame_mvs: self.use_prev_frame_mvs,
            prev_mi_grid: self.prev_mi_grid.clone(),
            frame_width: self.frame_width,
            frame_height: self.frame_height,
            resolved_refs: self.resolved_refs.clone(),
            counts: Counts::new(),
        }
    }

    /// Merges tile-column worker `w` (which decoded MI columns `[w.mi_col_start, w.mi_col_end)`
    /// into column-strip buffers) back into `self`: copies that column strip of every plane and
    /// of `mi_grid`, and sums the worker's probability-adaptation counts. Column strips are
    /// disjoint across workers, so the merged result is identical to a single-threaded decode
    /// (counts sum is order-independent). As before the strip buffers, only columns up to
    /// `mi_col_end` are copied -- the last column's superblock-rounded padding (which its strip
    /// also holds, see [`Self::spawn_column_worker`]) is dropped, staying zero in the merged
    /// frame exactly as in a sequential decode's output path.
    // VENDOR(uefi-player): column-strip merge, std-only (see the imports above).
    #[cfg(feature = "std")]
    pub(super) fn merge_column_worker(&mut self, w: &TileDecoder) {
        // Counts (order-independent integer sums).
        self.counts.add_assign(&w.counts);
        // mi_grid: MI columns [mi_col_start, mi_col_end) across all rows. The worker strip's
        // stride is its own `cols`; its first column is absolute column `mi_col_start`.
        let cols = self.mi_grid.cols();
        let rows = self.planes[0].height / 8;
        let (c0, c1) = (w.mi_col_start as usize, w.mi_col_end as usize);
        let src_cols = w.mi_grid.cols();
        for row in 0..rows {
            let base = row * cols;
            let src_base = row * src_cols;
            self.mi_grid.data[base + c0..base + c1]
                .copy_from_slice(&w.mi_grid.data[src_base..src_base + (c1 - c0)]);
        }
        // Planes: pixel columns [mi_col*8 >> sub_x, ...) across all rows, per plane. The worker
        // strip's stride is its own `width`; its first column is absolute column `px0`.
        for (p, plane) in self.planes.iter_mut().enumerate() {
            let sub = if p == 0 { 0 } else { self.subsampling_x };
            let px0 = (c0 * 8) >> sub;
            let px1 = (c1 * 8) >> sub;
            let width = plane.width;
            let dst = plane.as_mut_slice();
            let src = w.planes[p].as_slice();
            let src_width = w.planes[p].width;
            debug_assert_eq!(w.planes[p].x0, px0);
            let height = dst.len() / width;
            for row in 0..height {
                let base = row * width;
                let src_base = row * src_width;
                dst[base + px0..base + px1].copy_from_slice(&src[src_base..src_base + (px1 - px0)]);
            }
        }
    }

    /// Splits `data` (the concatenated tiles, each non-final one prefixed by a 4-byte big-endian
    /// size) into the `num_tiles` per-tile byte slices. Shared by the sequential loop in
    /// [`Self::decode_tiles`] (which iterates the returned slices) and the parallel path (which
    /// hands them to worker threads).
    pub(super) fn split_tiles(mut data: &[u8], num_tiles: usize) -> Result<Vec<&[u8]>, TileError> {
        let mut out = Vec::with_capacity(num_tiles);
        for i in 0..num_tiles {
            let last = i == num_tiles - 1;
            let size = if last {
                data.len()
            } else {
                if data.len() < 4 {
                    return Err(TileError::InvalidTileSize);
                }
                let (size_bytes, rest) = data.split_at(4);
                data = rest;
                u32::from_be_bytes(size_bytes.try_into().unwrap()) as usize
            };
            if data.len() < size {
                return Err(TileError::InvalidTileSize);
            }
            let (tile_bytes, rest) = data.split_at(size);
            out.push(tile_bytes);
            data = rest;
        }
        Ok(out)
    }

    /// Tile-parallel `decode_tiles` for the `tile_cols > 1 && tile_rows == 1` case: decode each
    /// tile column on its own worker thread (VP9 tile columns are independent), then merge each
    /// column strip back into `self`. Bit-identical to the sequential path (disjoint column
    /// writes; order-independent count sums). Uses `std::thread::scope` -- no external crate.
    // VENDOR(uefi-player): `thread::scope` driver, std-only (see the imports above).
    #[cfg(feature = "std")]
    pub(super) fn decode_tiles_parallel(
        &mut self,
        data: &[u8],
        tile_cols: u32,
        max_workers: usize,
    ) -> Result<(), TileError> {
        let tiles = Self::split_tiles(data, tile_cols as usize)?;

        // Bound concurrency (and the per-worker fixed overhead: counts + full-width above-context
        // arrays) to the machine's parallelism. Each worker's planes/mi_grid are column STRIPS
        // (see `spawn_column_worker`), so the strip buffers of all `tile_cols` workers sum to
        // ~one frame regardless of the declared tile count -- but a wide stream can still declare
        // hundreds of tile columns, and spawning that many threads (each with its own `Counts`)
        // at once is an attacker-controllable multiplier. The caller supplies the machine's
        // worker limit and has already ruled out the single-worker case, where the direct
        // sequential tile loop avoids worker allocation, thread spawn/join, and strip merge
        // overhead. Columns are independent, so chunk boundaries do not affect the result, and
        // the first error in column order is still what propagates.
        debug_assert!(max_workers > 1);
        let chunk = max_workers.min(tile_cols as usize) as u32;

        let mut c0 = 0u32;
        while c0 < tile_cols {
            let c1 = (c0 + chunk).min(tile_cols);
            let mut workers: Vec<TileDecoder> = (c0..c1)
                .map(|c| {
                    self.spawn_column_worker(
                        get_tile_offset(c, self.mi_cols, self.tile_cols_log2),
                        get_tile_offset(c + 1, self.mi_cols, self.tile_cols_log2),
                    )
                })
                .collect();

            let results: Vec<(
                Result<(), TileError>,
                [u64; crate::bench_timing::STAGE_COUNT],
            )> = std::thread::scope(|scope| {
                let handles: Vec<_> = workers
                    .iter_mut()
                    .zip(tiles[c0 as usize..c1 as usize].iter())
                    .map(|(w, &tile_bytes)| {
                        scope.spawn(move || {
                            crate::bench_timing::reset();
                            let result = w.decode_tile_bytes(tile_bytes);
                            (result, crate::bench_timing::snapshot())
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for (_, snapshot) in &results {
                crate::bench_timing::merge_snapshot(snapshot);
            }
            for (res, _) in results {
                res?;
            }
            for w in &workers {
                self.merge_column_worker(w);
            }
            c0 = c1;
        }
        Ok(())
    }
}
