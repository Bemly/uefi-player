//! Deblocking (loop) filter (spec §8.8 "Loop filter process").
//!
//! Spec §8.8's reference traversal of the frame is the following nested loops (taken directly from
//! the frame-wide traversal pseudocode at the top of the section); for parallelism the
//! implementation reorganizes it to plane-outermost, preserving each plane's order (see below):
//!
//! ```text
//! for ( row = 0; row < MiRows; row += 8 )
//!   for ( col = 0; col < MiCols; col += 8 )
//!     for ( plane = 0; plane < 3; plane++ )
//!       for ( pass = 0; pass < 2; pass++ )
//!         superblock loop filter process( plane, pass, row, col )
//! ```
//!
//! `pass == 0` means vertical edges (left/right block boundaries), and
//! `pass == 1` means horizontal edges (top/bottom block boundaries). Since
//! the same sample can be filtered multiple times, this traversal order
//! (vertical -> horizontal, superblock raster order) must be strictly
//! followed (see the NOTE in spec §8.8).
//!
//! For decode speed each plane is deblocked independently (the planes are disjoint buffers that
//! never read one another) and, above a size threshold, in parallel: within a plane the superblock
//! rows are filtered by a WAVEFRONT of worker threads (`wavefront_filter_planes`), each row
//! lagging the one above by two superblocks so their shared-corner writes stay ordered. This is
//! bit-exact -- the per-plane (row, col, pass) raster order above is preserved exactly; only
//! independent superblocks (and planes) ever run concurrently.
//!
//! # Known simplifications
//!
//! - `isIntra` (`RefFrames[row][col][0] <= INTRA_FRAME`) and `modeType`
//!   (whether `YModes` is `NEARESTMV`/`NEARMV`/`NEWMV`) are read from the
//!   real `ref_frame`/`y_mode` values added to `MiInfo` in M3 (see
//!   `superblock_loop_filter`).
//! - `loop_filter_ref_deltas`/`loop_filter_mode_deltas` persist across frames
//!   as spec §7.2 requires: `Decoder` (`lib.rs`) stores the previous frame's
//!   deltas and passes them into `parse_loop_filter_params`, which seeds from
//!   them (or from the default values `[1, 0, -1, -1]`/`[0, 0]` when
//!   `FrameIsIntra || error_resilient_mode`, i.e. `setup_past_independence`).

// VENDOR(uefi-player): core path works in both std and no_std builds.
// (`AtomicU32` is only used inside the std-only wavefront below, spelled fully
// qualified there, so it stays out of this import.)
// Only needed where threads or test knobs exist; without both it is unused.
#[cfg(any(feature = "std", feature = "test-support"))]
use core::sync::atomic::Ordering;

use crate::common::{clip3, get_uv_tx_size, round2, INTRA_FRAME, MAX_SEGMENTS};
use crate::framebuffer::Plane;
use crate::header::{LoopFilterParams, SegmentationParams, SEG_LVL_ALT_L};
use crate::prob_tables::{
    BLOCK_16X16, NEARESTMV, NEARMV, NEWMV, NUM_8X8_BLOCKS_HIGH_LOOKUP, NUM_8X8_BLOCKS_WIDE_LOOKUP,
    TX_16X16, TX_4X4, TX_8X8,
};
use crate::tile::MiGrid;

const MAX_REF_FRAMES: usize = 4;
const MAX_MODE_LF_DELTAS: usize = 2;
const MAX_LOOP_FILTER: i32 = 63;

/// Minimum frame size (in MI units, `MiCols * MiRows`) at which [`loop_filter_frame`] filters
/// the planes on separate threads. Below it the per-frame thread-spawn cost outweighs the win,
/// so the planes are filtered sequentially on the calling thread. Bench-tuned (see
/// `examples/bench.rs`); 4096 MI ~= 512x512 luma, so sub-VGA frames stay sequential.
const LF_PARALLEL_MIN_MI: u64 = 4096;

/// Test-only knob: forces [`loop_filter_frame`] down the sequential path even when the intra-plane
/// wavefront would engage, so `tests/loop_filter_parallel_test.rs` can assert the parallel and
/// sequential loop filter of a real decode are byte-identical. Compiled only for test builds (the
/// self-referential `test-support` dev-dependency), mirroring `tile::FORCE_SEQUENTIAL_TILES`.
#[cfg(feature = "test-support")]
// VENDOR(uefi-player): core path works in both std and no_std builds.
pub static FORCE_SEQUENTIAL_LOOP_FILTER: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Test-only knob: forces eligible frames to use at least two wavefront workers even on a
/// single-logical-CPU test host. See [`FORCE_SEQUENTIAL_LOOP_FILTER`].
#[cfg(feature = "test-support")]
// VENDOR(uefi-player): core path works in both std and no_std builds.
pub static FORCE_PARALLEL_LOOP_FILTER: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Test-only count of calls to [`wavefront_filter_planes`], used to reject a vacuous
/// parallel-vs-sequential equality test.
#[cfg(feature = "test-support")]
// VENDOR(uefi-player): core path works in both std and no_std builds.
pub static WAVEFRONT_INVOCATIONS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// `LvlLookup[ segmentId ][ ref ][ mode ]` (spec §8.8.1).
type LvlLookup = [[[i32; MAX_MODE_LF_DELTAS]; MAX_REF_FRAMES]; MAX_SEGMENTS];

/// Pixel read/write access to one plane for the loop filter. This abstracts the filter code over
/// the safe single-threaded [`Plane`] path (sequential / small frames and the per-plane threads)
/// and the raw `PlaneView` path used by the intra-plane wavefront, so the filter arithmetic is
/// written exactly once and each path keeps its own memory-safety story.
trait PlaneAccess {
    fn lf_get(&self, x: usize, y: usize) -> u16;
    fn lf_set(&mut self, x: usize, y: usize, v: u16);
    /// Base pointer of the row-major buffer, for the AVX2 kernels (which index it raw rather than
    /// going through `lf_get`/`lf_set`); `lf_stride` is its row stride (`= width`).
    // VENDOR(uefi-player): AVX2-only accessors, std-only (kernels are compiled out without it).
    #[cfg(feature = "std")]
    fn lf_base_ptr(&mut self) -> *mut u16;
    // VENDOR(uefi-player): AVX2-only accessors, std-only (see above).
    #[cfg(feature = "std")]
    fn lf_stride(&self) -> usize;
}

impl PlaneAccess for Plane {
    #[inline]
    fn lf_get(&self, x: usize, y: usize) -> u16 {
        self.get(x, y)
    }
    #[inline]
    fn lf_set(&mut self, x: usize, y: usize, v: u16) {
        self.set(x, y, v);
    }
    // VENDOR(uefi-player): AVX2-only accessors, std-only (see the trait).
    #[cfg(feature = "std")]
    #[inline]
    fn lf_base_ptr(&mut self) -> *mut u16 {
        self.as_mut_slice().as_mut_ptr()
    }
    // VENDOR(uefi-player): AVX2-only accessors, std-only (see the trait).
    #[cfg(feature = "std")]
    #[inline]
    fn lf_stride(&self) -> usize {
        self.width
    }
}

/// Raw-pointer view of one plane's pixel buffer for the multi-threaded loop-filter wavefront
/// ([`wavefront_filter_planes`]), where several worker threads write DISJOINT pixels of the *same*
/// plane at once. Rust forbids two `&mut Plane` (or overlapping `&mut [u16]`) to one buffer even
/// when the touched pixels are disjoint, so the workers share this `Copy` raw view instead; the
/// wavefront's per-row progress gate (a >=2 superblock-column lag with `Release`/`Acquire`) is what
/// actually keeps the accesses disjoint-or-ordered in time. The sequential path uses [`Plane`]
/// directly (fully safe) through the same [`PlaneAccess`] trait.
// VENDOR(uefi-player): wavefront-only view, std-only (workers share one buffer;
// the sequential path uses `Plane` directly).
#[cfg(feature = "std")]
#[derive(Clone, Copy)]
struct PlaneView {
    ptr: *mut u16,
    stride: usize,
    x0: usize,
    height: usize,
}

// SAFETY: `PlaneView` is a bare pointer into a `Plane`'s `Vec<u16>`. It is only created from a
// `&mut Plane` that outlives the `thread::scope` owning the workers (so the buffer stays alive and
// is never moved/reallocated for the view's lifetime), and shared with workers that -- by the
// wavefront ordering in `wavefront_filter_planes` -- never write the same pixel concurrently and
// always establish happens-before (the `Release`/`Acquire` progress counter) before reading a
// pixel another worker wrote. Hence there is no data race despite the shared buffer.
// VENDOR(uefi-player): wavefront-only, std-only (see `PlaneView` above).
#[cfg(feature = "std")]
unsafe impl Send for PlaneView {}
// VENDOR(uefi-player): wavefront-only, std-only (see `PlaneView` above).
#[cfg(feature = "std")]
unsafe impl Sync for PlaneView {}

// VENDOR(uefi-player): wavefront-only, std-only (see `PlaneView` above).
#[cfg(feature = "std")]
impl PlaneView {
    fn new(plane: &mut Plane) -> Self {
        let stride = plane.width;
        let x0 = plane.x0;
        let height = plane.height;
        Self {
            ptr: plane.as_mut_slice().as_mut_ptr(),
            stride,
            x0,
            height,
        }
    }

    #[inline]
    fn offset(&self, x: usize, y: usize) -> usize {
        debug_assert!(
            x >= self.x0 && x - self.x0 < self.stride && y < self.height,
            "PlaneView access out of range"
        );
        y * self.stride + (x - self.x0)
    }
}

// VENDOR(uefi-player): wavefront-only, std-only (see `PlaneView` above).
#[cfg(feature = "std")]
impl PlaneAccess for PlaneView {
    #[inline]
    fn lf_get(&self, x: usize, y: usize) -> u16 {
        // SAFETY: `offset` bounds-checks (debug). The wavefront guarantees any pixel this worker
        // reads is either its own or one another worker finished-and-published (Acquire) first, so
        // the read never races a concurrent write.
        unsafe { *self.ptr.add(self.offset(x, y)) }
    }
    #[inline]
    fn lf_set(&mut self, x: usize, y: usize, v: u16) {
        // SAFETY: `offset` bounds-checks (debug). The wavefront guarantees this worker has sole
        // access to (x, y) at this time (no other worker's superblock touches it concurrently).
        unsafe {
            *self.ptr.add(self.offset(x, y)) = v;
        }
    }
    #[inline]
    fn lf_base_ptr(&mut self) -> *mut u16 {
        self.ptr
    }
    #[inline]
    fn lf_stride(&self) -> usize {
        self.stride
    }
}

/// Spec §8.8.1 "Loop filter frame init process".
fn build_lvl_lookup(lf: &LoopFilterParams, seg: &SegmentationParams) -> LvlLookup {
    // nShift is computed once from the frame-level loop_filter_level
    // (note: not from lvlSeg — this matches the spec text).
    let n_shift = (lf.level as i32) >> 5;
    let mut table: LvlLookup = [[[0; MAX_MODE_LF_DELTAS]; MAX_REF_FRAMES]; MAX_SEGMENTS];

    for (segment_id, seg_table) in table.iter_mut().enumerate() {
        // Step 1-2 (spec §8.8.1): lvlSeg starts at loop_filter_level, then
        // seg_feature_active(SEG_LVL_ALT_L) overrides it (absolute or delta).
        let mut lvl_seg = lf.level as i32;
        if seg.enabled && seg.feature_enabled[segment_id][SEG_LVL_ALT_L] {
            let data = seg.feature_data[segment_id][SEG_LVL_ALT_L];
            lvl_seg = if seg.abs_or_delta_update {
                data
            } else {
                data + lf.level as i32
            };
            lvl_seg = clip3(0, MAX_LOOP_FILTER, lvl_seg);
        }

        if !lf.delta_enabled {
            for r in seg_table.iter_mut() {
                for m in r.iter_mut() {
                    *m = lvl_seg;
                }
            }
        } else {
            let intra_lvl = lvl_seg + (lf.ref_deltas[INTRA_FRAME as usize] as i32) * (1 << n_shift);
            seg_table[INTRA_FRAME as usize][0] = clip3(0, MAX_LOOP_FILTER, intra_lvl);
            // seg_table[INTRA_FRAME][1] is not defined by the spec (the INTRA_FRAME
            // row only has mode=0). On keyframes isIntra is always true and
            // modeType is always 0, so it's never referenced.
            for (r, ref_delta) in lf.ref_deltas.iter().enumerate().skip(1) {
                for (m, mode_delta) in lf.mode_deltas.iter().enumerate() {
                    let inter_lvl = lvl_seg
                        + (*ref_delta as i32) * (1 << n_shift)
                        + (*mode_delta as i32) * (1 << n_shift);
                    seg_table[r][m] = clip3(0, MAX_LOOP_FILTER, inter_lvl);
                }
            }
        }
    }
    table
}

/// The `limit`/`blimit`/`thresh` computation from spec §8.8.4 "Adaptive
/// filter strength process". (`lvl` is looked up from `LvlLookup` by the
/// caller and passed in.) The result is scaled by `<< (bit_depth - 8)` (identity at
/// `bit_depth == 8`) since these three values are compared directly against pixel
/// differences, which widen by the same factor at higher bit depths.
fn adaptive_filter_strength(lvl: i32, sharpness: u8, bit_depth: u8) -> (i32, i32, i32) {
    let shift = if sharpness > 4 {
        2
    } else if sharpness > 0 {
        1
    } else {
        0
    };
    let limit = if sharpness > 0 {
        clip3(1, 9 - sharpness as i32, lvl >> shift)
    } else {
        (lvl >> shift).max(1)
    };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    let depth_shift = (bit_depth - 8) as u32;
    (
        limit << depth_shift,
        blimit << depth_shift,
        thresh << depth_shift,
    )
}

/// Spec §8.8.3 "Filter size process".
#[allow(clippy::too_many_arguments)]
fn filter_size_process(
    tx_sz: u8,
    is_32_edge: bool,
    pass: u32,
    x: u32,
    y: u32,
    sub_x: u32,
    sub_y: u32,
    mi_cols: u32,
    mi_rows: u32,
) -> u8 {
    let base_size = if tx_sz == TX_4X4 && is_32_edge {
        TX_8X8
    } else {
        tx_sz.min(TX_16X16)
    };

    let luma_boundary = (pass == 0 && sub_x == 1 && (x >> 3) == mi_cols - 1)
        || (pass == 1 && sub_y == 1 && (y >> 3) == mi_rows - 1);
    if base_size == TX_16X16 && luma_boundary {
        TX_8X8
    } else {
        base_size
    }
}

/// Reads the sample at position `x + dx*k, y + dy*k` (the general form of the
/// spec's `q_k`/`p_k`: negative `k` denotes the `p` side, non-negative `k`
/// denotes the `q` side).
#[inline]
fn get_off<P: PlaneAccess>(plane: &P, x: usize, y: usize, dx: i64, dy: i64, k: i64) -> i32 {
    let px = (x as i64 + dx * k) as usize;
    let py = (y as i64 + dy * k) as usize;
    plane.lf_get(px, py) as i32
}

#[inline]
fn set_off<P: PlaneAccess>(plane: &mut P, x: usize, y: usize, dx: i64, dy: i64, k: i64, v: i32) {
    let px = (x as i64 + dx * k) as usize;
    let py = (y as i64 + dy * k) as usize;
    plane.lf_set(px, py, v as u16);
}

/// Spec §8.8.5.1 "Filter mask process". Returns `(hevMask, filterMask, flatMask, flatMask2)`.
/// `limit`/`blimit`/`thresh` arrive already scaled by `<< (bit_depth - 8)` (see
/// `adaptive_filter_strength`); the flat-mask threshold (fixed at `1` in the spec's 8-bit
/// text) is scaled here the same way.
#[allow(clippy::too_many_arguments)]
fn compute_filter_mask<P: PlaneAccess>(
    plane: &P,
    x: usize,
    y: usize,
    dx: i64,
    dy: i64,
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: u8,
    bit_depth: u8,
) -> (bool, bool, bool, bool) {
    let g = |k: i64| get_off(plane, x, y, dx, dy, k);
    let q0 = g(0);
    let q1 = g(1);
    let q2 = g(2);
    let q3 = g(3);
    let p0 = g(-1);
    let p1 = g(-2);
    let p2 = g(-3);
    let p3 = g(-4);

    let hev_mask = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;

    let mut mask = (p3 - p2).abs() > limit;
    mask |= (p2 - p1).abs() > limit;
    mask |= (p1 - p0).abs() > limit;
    mask |= (q1 - q0).abs() > limit;
    mask |= (q2 - q1).abs() > limit;
    mask |= (q3 - q2).abs() > limit;
    mask |= (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit;
    let filter_mask = !mask;

    let threshold = 1i32 << (bit_depth - 8);

    let mut flat_mask = false;
    if filter_size >= TX_8X8 {
        let mut m = (p1 - p0).abs() > threshold;
        m |= (q1 - q0).abs() > threshold;
        m |= (p2 - p0).abs() > threshold;
        m |= (q2 - q0).abs() > threshold;
        m |= (p3 - p0).abs() > threshold;
        m |= (q3 - q0).abs() > threshold;
        flat_mask = !m;
    }

    let mut flat_mask2 = false;
    if filter_size >= TX_16X16 {
        let q4 = g(4);
        let q5 = g(5);
        let q6 = g(6);
        let q7 = g(7);
        let p4 = g(-5);
        let p5 = g(-6);
        let p6 = g(-7);
        let p7 = g(-8);
        let mut m = (p7 - p0).abs() > threshold;
        m |= (q7 - q0).abs() > threshold;
        m |= (p6 - p0).abs() > threshold;
        m |= (q6 - q0).abs() > threshold;
        m |= (p5 - p0).abs() > threshold;
        m |= (q5 - q0).abs() > threshold;
        m |= (p4 - p0).abs() > threshold;
        m |= (q4 - q0).abs() > threshold;
        flat_mask2 = !m;
    }

    (hev_mask, filter_mask, flat_mask, flat_mask2)
}

/// Spec §8.8.5.2 "Narrow filter process" (`filter4`). The `0x80` base and the `clamp4`
/// range scale by bit depth (spec §8.8.5.2's `Round2(1, BitDepth-1)`-style base and the
/// `<< (BitDepth - 8)` clamp range); both are identity at `bit_depth == 8`.
fn narrow_filter<P: PlaneAccess>(
    plane: &mut P,
    x: usize,
    y: usize,
    dx: i64,
    dy: i64,
    hev_mask: bool,
    bit_depth: u8,
) {
    let half = 1i32 << (bit_depth - 1);
    let clamp_hi = (128i32 << (bit_depth - 8)) - 1;
    let clamp_lo = -(clamp_hi + 1);
    let clamp4 = |v: i32| clip3(clamp_lo, clamp_hi, v);

    let q0 = get_off(plane, x, y, dx, dy, 0);
    let q1 = get_off(plane, x, y, dx, dy, 1);
    let p0 = get_off(plane, x, y, dx, dy, -1);
    let p1 = get_off(plane, x, y, dx, dy, -2);

    let ps1 = p1 - half;
    let ps0 = p0 - half;
    let qs0 = q0 - half;
    let qs1 = q1 - half;

    let mut filter = if hev_mask { clamp4(ps1 - qs1) } else { 0 };
    filter = clamp4(filter + 3 * (qs0 - ps0));
    let filter1 = clamp4(filter + 4) >> 3;
    let filter2 = clamp4(filter + 3) >> 3;

    let oq0 = clamp4(qs0 - filter1) + half;
    let op0 = clamp4(ps0 + filter2) + half;
    set_off(plane, x, y, dx, dy, 0, oq0);
    set_off(plane, x, y, dx, dy, -1, op0);

    if !hev_mask {
        let filter = round2(filter1, 1);
        let oq1 = clamp4(qs1 - filter) + half;
        let op1 = clamp4(ps1 + filter) + half;
        set_off(plane, x, y, dx, dy, 1, oq1);
        set_off(plane, x, y, dx, dy, -2, op1);
    }
}

/// Spec §8.8.5.3 "Wide filter process". `log2_size` is 3 (8-tap) or 4 (16-tap).
fn wide_filter<P: PlaneAccess>(
    plane: &mut P,
    x: usize,
    y: usize,
    dx: i64,
    dy: i64,
    log2_size: u32,
) {
    let n: i64 = (1i64 << (log2_size - 1)) - 1;
    // F's index is -n..n-1. Stored in a 0-based array by adding offset n.
    let mut f = [0i32; 16];

    let mut i = -n;
    while i < n {
        let mut t = get_off(plane, x, y, dx, dy, i);
        let mut j = -n;
        while j <= n {
            let p = clip3(-((n + 1) as i32), n as i32, (i + j) as i32) as i64;
            t += get_off(plane, x, y, dx, dy, p);
            j += 1;
        }
        f[(i + n) as usize] = round2(t, log2_size);
        i += 1;
    }

    let mut i = -n;
    while i < n {
        set_off(plane, x, y, dx, dy, i, f[(i + n) as usize]);
        i += 1;
    }
}

/// Spec §8.8.5 "Sample filtering process".
#[allow(clippy::too_many_arguments)]
fn sample_filtering<P: PlaneAccess>(
    plane: &mut P,
    x: usize,
    y: usize,
    dx: i64,
    dy: i64,
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: u8,
    bit_depth: u8,
) {
    let (hev_mask, filter_mask, flat_mask, flat_mask2) = compute_filter_mask(
        plane,
        x,
        y,
        dx,
        dy,
        limit,
        blimit,
        thresh,
        filter_size,
        bit_depth,
    );

    if !filter_mask {
        return;
    }
    if filter_size == TX_4X4 || !flat_mask {
        narrow_filter(plane, x, y, dx, dy, hev_mask, bit_depth);
    } else if filter_size == TX_8X8 || !flat_mask2 {
        wide_filter(plane, x, y, dx, dy, 3);
    } else {
        wide_filter(plane, x, y, dx, dy, 4);
    }
}

/// Per-position filter gating/strength: `(x, y, apply_filter, lvl, filter_size)`, spec
/// §8.8.2 steps 4-10 plus the §8.8.3/§8.8.4 lookups. Factored out of
/// `superblock_loop_filter`'s inner loop so the scalar loop and the AVX2 fast path
/// (`superblock_loop_filter_edge_avx2`, below) call the exact same code to decide
/// WHICH positions get filtered and how strongly -- the two can never diverge on that,
/// only on how the pixel arithmetic itself is carried out.
#[allow(clippy::too_many_arguments)]
fn edge_position_params(
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lvl_lookup: &LvlLookup,
    plane_idx: usize,
    pass: u32,
    row: u32,
    col: u32,
    sub_x: u32,
    sub_y: u32,
    sub: u32,
    edge: u32,
    i: u32,
) -> (u32, u32, bool, i32, u8) {
    let (x, y): (u32, u32) = if pass == 0 {
        (col * 8 + edge * (4 << sub_x), row * 8 + (i << sub_y))
    } else {
        (col * 8 + (i << sub_x), row * 8 + edge * (4 << sub_y))
    };

    let loop_col = ((x >> 3) >> sub_x) << sub_x;
    let loop_row = ((y >> 3) >> sub_y) << sub_y;
    let mi = mi_grid.get(loop_row, loop_col);

    let mi_size = mi.mi_size;
    let tx_size = mi.tx_size;
    let tx_sz = if plane_idx > 0 {
        get_uv_tx_size(mi_size, tx_size, subsampling_x, subsampling_y)
    } else {
        tx_size
    };
    let sb_size = if sub == 0 {
        mi_size
    } else {
        mi_size.max(BLOCK_16X16)
    };
    let skip = mi.skip;
    // Spec §8.8.2 step 9: isIntra = RefFrames[loopRow][loopCol][0] <= INTRA_FRAME.
    let ref_frame = mi.ref_frame[0];
    let is_intra = ref_frame == INTRA_FRAME;

    let is_block_edge = if pass == 0 {
        x % (8 * NUM_8X8_BLOCKS_WIDE_LOOKUP[sb_size as usize] as u32) == 0
    } else {
        y % (8 * NUM_8X8_BLOCKS_HIGH_LOOKUP[sb_size as usize] as u32) == 0
    };

    let is_tx_edge =
        if pass == 1 && sub_x == 1 && mi_cols % 2 == 1 && edge % 2 == 1 && (x + 8) >= mi_cols * 8 {
            false
        } else {
            edge.is_multiple_of(1 << tx_sz)
        };

    let is_32_edge = edge.is_multiple_of(8);

    let on_screen =
        !(x >= 8 * mi_cols || y >= 8 * mi_rows || (pass == 0 && x == 0) || (pass == 1 && y == 0));

    let apply_filter = on_screen && (is_block_edge || (is_tx_edge && (is_intra || !skip)));

    let filter_size = filter_size_process(
        tx_sz, is_32_edge, pass, x, y, sub_x, sub_y, mi_cols, mi_rows,
    );

    // Spec §8.8.4: modeType = 1 if mode in {NEARESTMV,NEARMV,NEWMV} else 0.
    let mode_type = matches!(mi.y_mode, NEARESTMV | NEARMV | NEWMV) as usize;
    let lvl = lvl_lookup[mi.segment_id as usize][ref_frame as usize][mode_type];

    (x, y, apply_filter, lvl, filter_size)
}

/// Spec §8.8.2 "Superblock loop filter process".
#[allow(clippy::too_many_arguments)]
fn superblock_loop_filter<P: PlaneAccess>(
    plane: &mut P,
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lvl_lookup: &LvlLookup,
    sharpness: u8,
    plane_idx: usize,
    pass: u32,
    row: u32,
    col: u32,
    bit_depth: u8,
) {
    let (sub_x, sub_y) = if plane_idx == 0 {
        (0u32, 0u32)
    } else {
        (subsampling_x, subsampling_y)
    };

    let (dx, dy, sub, edge_len): (i64, i64, u32, u32) = if pass == 0 {
        (1, 0, sub_x, 64 >> sub_y)
    } else {
        (0, 1, sub_y, 64 >> sub_x)
    };

    let num_edges = 16u32 >> sub;
    for edge in 0..num_edges {
        // AVX2 fast path (both passes, all bit depths): the kernels' 8-bit constants (128,
        // +/-128, flat threshold 1) scale by `<< (bit_depth - 8)` inside the kernel, and the
        // limit/blimit/thresh strengths are bit-depth-computed by `adaptive_filter_strength`
        // below. pass==1 (horizontal) has the along-edge axis contiguous in memory (SIMD wave 3);
        // pass==0 (vertical) transposes the tap window into that same layout (SIMD wave 4, see
        // `superblock_loop_filter_edge_avx2` / `simd::loop_filter_vert8_avx2`). Both decide
        // WHICH positions filter and how strongly via the same `edge_position_params` the scalar
        // loop uses -- only the pixel arithmetic is vectorized.
        // VENDOR(uefi-player): AVX2 dispatch needs `std` (see simd.rs); without
        // it this block is compiled out and the scalar loop below always runs.
        #[cfg(all(feature = "std", target_arch = "x86_64"))]
        if crate::simd::avx2_enabled() {
            superblock_loop_filter_edge_avx2(
                plane,
                mi_grid,
                mi_cols,
                mi_rows,
                subsampling_x,
                subsampling_y,
                lvl_lookup,
                sharpness,
                plane_idx,
                pass,
                row,
                col,
                sub_x,
                sub_y,
                sub,
                edge_len,
                edge,
                bit_depth,
            );
            continue;
        }

        for i in 0..edge_len {
            let (x, y, apply_filter, lvl, filter_size) = edge_position_params(
                mi_grid,
                mi_cols,
                mi_rows,
                subsampling_x,
                subsampling_y,
                lvl_lookup,
                plane_idx,
                pass,
                row,
                col,
                sub_x,
                sub_y,
                sub,
                edge,
                i,
            );

            if apply_filter && lvl > 0 {
                let (limit, blimit, thresh) = adaptive_filter_strength(lvl, sharpness, bit_depth);
                sample_filtering(
                    plane,
                    (x >> sub_x) as usize,
                    (y >> sub_y) as usize,
                    dx,
                    dy,
                    limit,
                    blimit,
                    thresh,
                    filter_size,
                    bit_depth,
                );
            }
        }
    }
}

/// AVX2 fast path for one edge, either pass: batches `edge_len` (always a multiple of 8)
/// along-edge positions into groups of 8 -- for pass==1 (horizontal edges) 8 contiguous plane
/// COLUMNS dispatched to `simd::loop_filter_horiz8_avx2` (SIMD wave 3); for pass==0 (vertical
/// edges) 8 consecutive plane ROWS (that orientation's along-edge axis) dispatched to
/// `simd::loop_filter_vert8_avx2`, which transposes the tap window and reuses the horizontal
/// kernel (SIMD wave 4). WHICH positions get filtered and how strongly is decided by the exact
/// same `edge_position_params` the scalar loop uses -- only the pixel arithmetic is vectorized.
/// All three filter sizes (TX_4X4 narrow / TX_8X8 wide8 / TX_16X16 wide2) are batched; the
/// kernel's per-lane `is_tx8`/`is_tx16` masks pick each lane's filter, mirroring
/// `sample_filtering`'s three-way branch (see the current SIMD summary in
/// `docs/implementation-notes.md`).
// VENDOR(uefi-player): AVX2 dispatcher, std-only (see the call site above).
#[cfg(all(feature = "std", target_arch = "x86_64"))]
#[allow(clippy::too_many_arguments)]
fn superblock_loop_filter_edge_avx2<P: PlaneAccess>(
    plane: &mut P,
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lvl_lookup: &LvlLookup,
    sharpness: u8,
    plane_idx: usize,
    pass: u32,
    row: u32,
    col: u32,
    sub_x: u32,
    sub_y: u32,
    sub: u32,
    edge_len: u32,
    edge: u32,
    bit_depth: u8,
) {
    let mut i = 0u32;
    while i < edge_len {
        let mut eligible = [0i32; 8];
        let mut is_tx8 = [0i32; 8];
        let mut is_tx16 = [0i32; 8];
        let mut limit = [0i32; 8];
        let mut blimit = [0i32; 8];
        let mut thresh = [0i32; 8];
        let mut x0 = 0usize;
        let mut y0 = 0usize;

        for lane in 0..8u32 {
            let (x, y, apply_filter, lvl, filter_size) = edge_position_params(
                mi_grid,
                mi_cols,
                mi_rows,
                subsampling_x,
                subsampling_y,
                lvl_lookup,
                plane_idx,
                pass,
                row,
                col,
                sub_x,
                sub_y,
                sub,
                edge,
                i + lane,
            );
            let px = (x >> sub_x) as usize;
            let py = (y >> sub_y) as usize;
            if lane == 0 {
                x0 = px;
                y0 = py;
            }
            if pass == 1 {
                debug_assert_eq!(py, y0, "pass==1 edge: y is constant along the edge");
                debug_assert_eq!(px, x0 + lane as usize, "pass==1 edge: x is contiguous");
            } else {
                debug_assert_eq!(px, x0, "pass==0 edge: x is constant along the edge");
                debug_assert_eq!(py, y0 + lane as usize, "pass==0 edge: y is contiguous");
            }

            if apply_filter && lvl > 0 {
                // Runs at all bit depths: `adaptive_filter_strength` scales its 8-bit
                // constants by `<< (bit_depth - 8)` (identity at 8-bit).
                eligible[lane as usize] = -1;
                is_tx8[lane as usize] = -((filter_size == TX_8X8) as i32);
                is_tx16[lane as usize] = -((filter_size == TX_16X16) as i32);
                let (l, bl, th) = adaptive_filter_strength(lvl, sharpness, bit_depth);
                limit[lane as usize] = l;
                blimit[lane as usize] = bl;
                thresh[lane as usize] = th;
            }
        }

        if eligible.iter().any(|&e| e != 0) {
            let plane_width = plane.lf_stride();
            // Raw base pointer of the plane buffer for the kernels. In the sequential path this is
            // `Plane::as_mut_slice().as_mut_ptr()`; in the wavefront path it is the shared
            // `PlaneView` pointer (see `PlaneView`'s safety note). Either way the kernel touches
            // only the wavefront-exclusive window documented at each call below.
            let base = plane.lf_base_ptr();
            if pass == 1 {
                // SAFETY: `avx2_enabled()` was checked by the caller (`superblock_loop_filter`).
                // The row window the kernel touches at columns x0..x0+8 (pass==1's dx=0,dy=1 taps
                // run in the row direction) is exactly the window the already-proven-bit-exact
                // scalar `compute_filter_mask`/`wide_filter` reads for an eligible lane at this
                // same edge-constant `y0`: rows y0-4..=y0+3 always, and -- only when an eligible
                // TX_16X16 lane is present (`is_tx16` is set only for eligible lanes) -- the wider
                // y0-8..=y0+7 the wide16 filter needs. Both are in-bounds whenever the respective
                // lane is eligible; `planes` are allocated out to superblock boundaries (see
                // `framebuffer::Plane`'s doc comment), so the extra columns read/written for the
                // group's other lanes are always valid memory too.
                unsafe {
                    crate::simd::loop_filter_horiz8_avx2(
                        base,
                        plane_width,
                        x0,
                        y0,
                        &eligible,
                        &is_tx8,
                        &is_tx16,
                        &limit,
                        &blimit,
                        &thresh,
                        bit_depth,
                    );
                }
            } else {
                // SAFETY: `avx2_enabled()` was checked by the caller (`superblock_loop_filter`).
                // The column window the kernel touches at rows y0..y0+8 (pass==0's dx=1,dy=0 taps
                // run in the column direction) is exactly what the already-proven-bit-exact scalar
                // `compute_filter_mask`/`wide_filter` reads for an eligible lane at this same
                // edge-constant `x0`: columns x0-4..=x0+3 always, and -- only when an eligible
                // TX_16X16 lane is present (`is_tx16` set only for eligible lanes) -- the wider
                // x0-8..=x0+7 the wide16 filter needs. Both are in-bounds whenever the respective
                // lane is eligible; planes are allocated out to superblock boundaries in both
                // dimensions (see `framebuffer::Plane`), so the extra rows read/written for the
                // group's other lanes are valid memory too (the write blends in the original
                // value there).
                unsafe {
                    crate::simd::loop_filter_vert8_avx2(
                        base,
                        plane_width,
                        x0,
                        y0,
                        &eligible,
                        &is_tx8,
                        &is_tx16,
                        &limit,
                        &blimit,
                        &thresh,
                        bit_depth,
                    );
                }
            }
        }

        i += 8;
    }
}

/// Filters one plane over the whole frame sequentially in the spec's per-plane order (superblock
/// raster; vertical then horizontal pass). The single-threaded fallback used by
/// [`loop_filter_frame`] for small frames / single-core machines; larger frames filter each plane
/// with [`wavefront_filter_planes`] instead.
#[allow(clippy::too_many_arguments)]
fn loop_filter_plane(
    plane: &mut Plane,
    plane_idx: usize,
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lvl_lookup: &LvlLookup,
    sharpness: u8,
    bit_depth: u8,
) {
    let mut row = 0u32;
    while row < mi_rows {
        let mut col = 0u32;
        while col < mi_cols {
            for pass in 0..2u32 {
                superblock_loop_filter(
                    plane,
                    mi_grid,
                    mi_cols,
                    mi_rows,
                    subsampling_x,
                    subsampling_y,
                    lvl_lookup,
                    sharpness,
                    plane_idx,
                    pass,
                    row,
                    col,
                    bit_depth,
                );
            }
            col += 8;
        }
        row += 8;
    }
}

/// Filters all three planes using the intra-plane WAVEFRONT in a SINGLE `thread::scope`: each of
/// `n_threads` workers processes a round-robin subset of superblock rows, in the spec's per-plane
/// order (vertical then horizontal pass per superblock), advancing through plane 0, then 1, then 2.
///
/// A worker may filter superblock `(r, c)` of a plane only once that plane's row `r-1` has finished
/// column `c+2` -- a **2-superblock-column lag**. One column would not be enough: `(r, c)`'s
/// horizontal (top-edge) pass and `(r-1, c+1)`'s vertical (left-edge) pass both write a shared
/// 8-sample corner where the superblocks meet, so with only a 1-column lead the row above could
/// still be writing that corner. Each plane has its own `progress` counter array (superblock
/// columns fully filtered per row); the `Release` store / `Acquire` load also publishes the pixels
/// a worker wrote before the dependent worker reads them.
///
/// Bit-identical to the sequential per-plane raster order: every read/write dependency (left
/// neighbour via same-thread ordering, above neighbour via the gate) is preserved. The three planes
/// are independent, so fusing them into one scope (rather than three) changes only timing -- a
/// worker that runs out of work in one plane flows straight into the next instead of idling at a
/// per-plane barrier, and the per-frame thread spawns are paid once, not once per plane.
// VENDOR(uefi-player): `thread::scope` wavefront, std-only; without it the
// sequential per-plane loop in `loop_filter_frame` always runs (bit-identical).
#[cfg(feature = "std")]
#[allow(clippy::too_many_arguments)]
fn wavefront_filter_planes(
    planes: &mut [Plane; 3],
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lvl_lookup: &LvlLookup,
    sharpness: u8,
    bit_depth: u8,
    n_threads: usize,
) {
    #[cfg(feature = "test-support")]
    WAVEFRONT_INVOCATIONS.fetch_add(1, Ordering::Relaxed);

    let n_sb_rows = mi_rows.div_ceil(8) as usize;
    let n_sb_cols = mi_cols.div_ceil(8) as usize;
    // One `progress` array per plane: `progress[p][r]` = superblock columns fully filtered (both
    // passes) in row `r` of plane `p`. Published with `Release`, awaited with `Acquire`.
    // VENDOR(uefi-player): core path works in both std and no_std builds.
    let progress: [Vec<core::sync::atomic::AtomicU32>; 3] =
        core::array::from_fn(|_| (0..n_sb_rows).map(|_| core::sync::atomic::AtomicU32::new(0)).collect());
    let progress = &progress;
    let [p0, p1, p2] = planes;
    let views = [PlaneView::new(p0), PlaneView::new(p1), PlaneView::new(p2)];

    std::thread::scope(|s| {
        for t in 0..n_threads {
            s.spawn(move || {
                // Advance through the planes in order; each is an independent wavefront, so a worker
                // that runs out of its rows in plane `p` flows straight into `p + 1` (no barrier).
                for plane_idx in 0..3usize {
                    // Private `Copy` of this plane's shared raw view; writes go to the one
                    // underlying buffer, kept disjoint-in-time by the gate below.
                    let mut view = views[plane_idx];
                    let prog = &progress[plane_idx];
                    let mut sb_row = t;
                    while sb_row < n_sb_rows {
                        let row = (sb_row * 8) as u32;
                        for sb_col in 0..n_sb_cols {
                            // Wavefront gate: wait until this plane's row above has finished column
                            // `sb_col + 1` (the 2-superblock lag). Row 0 has no row above.
                            if sb_row > 0 {
                                let need = (sb_col as u32 + 2).min(n_sb_cols as u32);
                                while prog[sb_row - 1].load(Ordering::Acquire) < need {
                                    // VENDOR(uefi-player): core path, both builds.
                                    core::hint::spin_loop();
                                }
                            }
                            let col = (sb_col * 8) as u32;
                            for pass in 0..2u32 {
                                superblock_loop_filter(
                                    &mut view,
                                    mi_grid,
                                    mi_cols,
                                    mi_rows,
                                    subsampling_x,
                                    subsampling_y,
                                    lvl_lookup,
                                    sharpness,
                                    plane_idx,
                                    pass,
                                    row,
                                    col,
                                    bit_depth,
                                );
                            }
                            // Publish: this superblock (both passes) is done; unblock the row below.
                            prog[sb_row].store(sb_col as u32 + 1, Ordering::Release);
                        }
                        sb_row += n_threads;
                    }
                }
            });
        }
    });
}

/// Frame-wide entry point for spec §8.8 "Loop filter process".
///
/// `planes` is `CurrFrame` (equivalent to `TileDecoder::planes`/`planes_mut`,
/// a buffer allocated out to superblock boundaries); `mi_grid` is the mode
/// info grid; `mi_cols`/`mi_rows` is the non-padded, mi-unit frame size
/// obtained from `compute_image_size()`.
#[allow(clippy::too_many_arguments)]
pub fn loop_filter_frame(
    planes: &mut [Plane; 3],
    mi_grid: &MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    subsampling_x: u32,
    subsampling_y: u32,
    lf: &LoopFilterParams,
    seg: &SegmentationParams,
    bit_depth: u8,
) {
    let lvl_lookup = build_lvl_lookup(lf, seg);
    let sharpness = lf.sharpness;

    // Each plane is deblocked independently (disjoint buffers; `mi_grid`/`lvl_lookup` are shared
    // read-only). Above `LF_PARALLEL_MIN_MI` each plane is filtered with the intra-plane wavefront
    // (see `wavefront_filter_planes`) across `n_threads` workers; below it -- or with no usable
    // parallelism -- the planes are filtered sequentially on this thread. Both paths preserve every
    // plane's (row, col, pass) raster order spec §8.8 requires, so the output is bit-identical.
    // Test-only override: force the sequential path so the parallel/sequential equality test can
    // use the wavefront-engaging clips as their own reference.
    #[cfg(feature = "test-support")]
    let force_sequential = FORCE_SEQUENTIAL_LOOP_FILTER.load(Ordering::Relaxed);
    #[cfg(not(feature = "test-support"))]
    let force_sequential = false;
    #[cfg(feature = "test-support")]
    let force_parallel = FORCE_PARALLEL_LOOP_FILTER.load(Ordering::Relaxed);
    #[cfg(not(feature = "test-support"))]
    let force_parallel = false;

    // Cheap gates first, so only frames that actually parallelize pay for the
    // `available_parallelism()` query; `n_threads == 1` then funnels into the sequential path below.
    let n_threads = if force_sequential
        || (!force_parallel && (mi_cols as u64) * (mi_rows as u64) < LF_PARALLEL_MIN_MI)
    {
        1
    } else {
        let n_sb_rows = mi_rows.div_ceil(8) as usize;
        // One worker per superblock row is the most the wavefront can use (more would idle); cap at
        // the machine's parallelism. The test-only override raises the floor to two workers so
        // the equality test cannot silently exercise the sequential path on a one-CPU host.
        // VENDOR(uefi-player): without `std` there is no thread pool to query;
        // 1 funnels into the sequential path above.
        #[cfg(feature = "std")]
        {
            let available = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let available = if force_parallel {
                available.max(2)
            } else {
                available
            };
            available.min(n_sb_rows.max(1))
        }
        #[cfg(not(feature = "std"))]
        {
            let _ = (n_sb_rows, force_parallel);
            1
        }
    };

    if n_threads <= 1 {
        for (plane_idx, plane) in planes.iter_mut().enumerate() {
            loop_filter_plane(
                plane,
                plane_idx,
                mi_grid,
                mi_cols,
                mi_rows,
                subsampling_x,
                subsampling_y,
                &lvl_lookup,
                sharpness,
                bit_depth,
            );
        }
        return;
    }

    // All three planes are filtered in one fused wavefront pass (see `wavefront_filter_planes`): a
    // single thread::scope whose workers flow across the independent planes, so no worker idles at a
    // per-plane barrier and the per-frame thread spawns are paid once, not once per plane.
    // VENDOR(uefi-player): std-only (see `wavefront_filter_planes`); without it
    // `n_threads` is 1 and the sequential path above has already returned.
    #[cfg(feature = "std")]
    wavefront_filter_planes(
        planes,
        mi_grid,
        mi_cols,
        mi_rows,
        subsampling_x,
        subsampling_y,
        &lvl_lookup,
        sharpness,
        bit_depth,
        n_threads,
    );
}

#[cfg(test)]
#[path = "../tests/unit/loop_filter.rs"]
mod tests;
