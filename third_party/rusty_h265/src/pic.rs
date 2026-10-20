//! Per-picture decoding state at 4×4 (minimum transform block) granularity:
//! the z-scan order table (§6.5.2), neighbour availability (§6.4.1), and the
//! per-block maps the syntax, intra prediction and loop filters read.

// VENDOR(uefi-player): alloc::sync::Arc is the same type in both builds.
use alloc::sync::Arc;
use crate::ps::{Sps, TileLayout};
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{vec, vec::Vec};

/// `CuPredMode` per 4×4.
pub const PRED_NONE: u8 = 0;
pub const PRED_INTRA: u8 = 1;
pub const PRED_INTER: u8 = 2;
pub const PRED_SKIP: u8 = 3;

/// SAO parameters of one CTB for one component.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaoParams {
    /// 0 = off, 1 = band, 2 = edge.
    pub type_idx: u8,
    /// Band position or edge-offset class.
    pub aux: u8,
    /// Offsets (band: 4 consecutive bands from `aux`; edge: categories 1..=4).
    pub offset: [i16; 4],
}

/// Motion data of one 4×4 (Phase 3 fills it; deblocking reads it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Motion {
    pub mv: [[i16; 2]; 2],
    pub ref_idx: [i8; 2],
    /// Bit 0 = L0 used, bit 1 = L1 used.
    pub pred_flags: u8,
    /// POC of the referenced pictures (for boundary strength: "same picture" test).
    pub ref_poc: [i32; 2],
    /// Bit l set = the list-l reference was a long-term picture when this
    /// picture was decoded (`LongTermRefPic` for TMVP).
    pub ref_lt: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CtbFilterParams {
    pub deblock_disabled: bool,
    pub beta_offset_div2: i8,
    pub tc_offset_div2: i8,
    pub lf_across_slices: bool,
    pub cb_qp_offset: i8,
    pub cr_qp_offset: i8,
    pub sao_luma: bool,
    pub sao_chroma: bool,
}

pub struct PicState {
    pub width: usize,
    pub height: usize,
    /// Size in 4×4 units.
    pub w4: usize,
    pub h4: usize,
    pub log2_ctb: usize,
    pub ctb_w: usize,
    pub ctb_h: usize,
    /// `MinTbAddrZs` at 4×4 granularity.
    ///
    /// Shared, not owned: it is a function of the SPS and the tile layout only,
    /// so it is identical for every picture in a coded video sequence. Built
    /// per picture it cost a nested loop over every 4x4 in the frame -- 57,600
    /// of them at 720p, 600 times over for a 600-picture clip.
    pub zs: Arc<[u32]>,

    /// Per CTB (raster): `SliceAddrRs` of the slice containing it, or -1.
    ///
    /// NOT fused with `tile_id` into one key. §6.4.1 asks one question of a
    /// neighbouring CTB -- same slice AND same tile -- so packing both into a
    /// `u64` would make it one load, one bounds check and one compare where
    /// there are two, two and three, and would subsume the `< 0` test as well.
    /// Built twice and measured twice: ALONGSIDE these arrays it cost
    /// `coding_quadtree` +164 instructions, and REPLACING them (with accessors
    /// unpacking the halves for the loop filters) +259. The fusion itself is
    /// what does not pay, not the extra array.
    pub slice_addr: Vec<i32>,
    /// Per CTB (raster): index into the picture's slice header list.
    pub ctb_slice: Vec<u16>,
    /// Per CTB (raster): the CTB was decoded and every one of its samples
    /// written. Set AFTER `decode_ctu` returns, so a slice that fails partway
    /// through a CTB leaves it false. `Picture::reuse` no longer clears the
    /// sample planes, and this is what says which of them still hold the
    /// previous picture -- see `Decoder::clear_uncovered`.
    pub ctb_done: Vec<bool>,
    /// Per CTB (raster): tile id. Shared for the same reason as [`zs`](Self::zs).
    pub tile_id: Arc<[u32]>,
    /// Per 4×4: `CuPredMode` (PRED_*).
    pub pred_mode: Vec<u8>,
    /// Per 4×4: luma intra prediction mode.
    pub intra_mode: Vec<u8>,
    /// Per 4×4: `QpY`.
    pub qp_y: Vec<i8>,
    /// Per 4×4: coding quadtree depth (split_cu_flag context).
    pub ct_depth: Vec<u8>,
    /// Per 4×4: bit0 = left edge is a TU edge, bit1 = top edge is a TU edge,
    /// bit2 = left edge is a PU edge, bit3 = top edge is a PU edge.
    pub edges: Vec<u8>,
    /// Per 4×4: luma transform block has non-zero coefficients.
    pub nz: Vec<u8>,
    /// Per 4×4: samples must not be touched by the loop filters
    /// (`pcm_loop_filter_disabled_flag && pcm_flag`, or `cu_transquant_bypass_flag`).
    pub filter_bypass: Vec<u8>,
    /// Per 4×4: motion.
    pub motion: Vec<Motion>,
    /// Per CTB: SAO parameters for Y, Cb, Cr.
    pub sao: Vec<[SaoParams; 3]>,
    /// Per CTB: the slice-level filter parameters that apply to it.
    pub ctb_filter: Vec<CtbFilterParams>,
}

/// The half of a §6.4.1 availability test that depends only on the CURRENT
/// block: its z-scan order, slice address and tile id.
///
/// `PicState::available` recomputed all three on every neighbour query --
/// `idx4` and `ctb_of` each scale by a runtime stride, so that is two
/// multiplies and two loads -- and every caller asks about two or three
/// neighbours of the SAME block. Hoisting the current-block half leaves the
/// per-neighbour test with one multiply for `idx4`, one for `ctb_of`, and
/// three compares.
#[derive(Clone, Copy)]
pub struct AvailAt {
    zs: u32,
    slice: i32,
    tile: u32,
}

/// The sequence-invariant half of [`PicState`].
///
/// `MinTbAddrZs` and the raster-order tile map are functions of the SPS and the
/// tile layout alone, so they are the same for every picture of a coded video
/// sequence. Building them per picture put a nested loop over every 4x4 block
/// in the frame on the per-picture path: measured at 13.7% of decode for the
/// whole per-picture setup, of which this was the compute half.
pub struct SeqTables {
    pub zs: Arc<[u32]>,
    pub tile_id: Arc<[u32]>,
    /// What these were built from, so a cache can tell when they are stale.
    ///
    /// BOTH tile vectors, not just `rs_to_ts`. Keying on `rs_to_ts` alone looks
    /// sufficient -- surely a different tile grid reorders the scan -- and is
    /// not: `PPS_A_qualcomm_7` switches PPS to a layout with the SAME raster-to
    /// -tile-scan map and a DIFFERENT tile numbering, so the cache hit, the
    /// stale `tile_id` made `first_in_tile` wrong, and the parse died on
    /// `end_of_subset_one_bit`. If a table is built from two inputs, key it on
    /// two inputs.
    key: (usize, usize, usize, usize, Vec<u32>, Vec<u32>),
}

impl SeqTables {
    pub fn build(sps: &Sps, tiles: &TileLayout) -> SeqTables {
        let w4 = (sps.width as usize).div_ceil(4);
        let h4 = (sps.height as usize).div_ceil(4);
        let log2_ctb = sps.log2_ctb_size as usize;
        let ctb_w = sps.pic_width_in_ctbs as usize;
        let ctb_h = sps.pic_height_in_ctbs as usize;
        let nctb = ctb_w * ctb_h;
        // (6-10) at 4x4 granularity.
        let mut zs = vec![0u32; w4 * h4];
        let shift = log2_ctb - 2;
        for y in 0..h4 {
            for x in 0..w4 {
                let ctb_rs = ctb_w * (y >> shift) + (x >> shift);
                let mut v = tiles.rs_to_ts[ctb_rs] << (shift * 2);
                for i in 0..shift {
                    let m = 1usize << i;
                    if m & x != 0 {
                        v += (m * m) as u32;
                    }
                    if m & y != 0 {
                        v += (2 * m * m) as u32;
                    }
                }
                zs[y * w4 + x] = v;
            }
        }
        let mut tile_id = vec![0u32; nctb];
        for (rs, t) in tile_id.iter_mut().enumerate() {
            *t = tiles.tile_id[tiles.rs_to_ts[rs] as usize];
        }
        SeqTables {
            zs: zs.into(),
            tile_id: tile_id.into(),
            key: (w4, h4, log2_ctb, ctb_w, tiles.rs_to_ts.clone(), tiles.tile_id.clone()),
        }
    }

    /// Whether these tables still describe this SPS and tile layout. Comparing
    /// the two tile vectors is a walk over the CTBs -- 240 at 720p -- against
    /// rebuilding, which walks every 4x4 block, 57,600 of them.
    pub fn matches(&self, sps: &Sps, tiles: &TileLayout) -> bool {
        self.key.0 == (sps.width as usize).div_ceil(4)
            && self.key.1 == (sps.height as usize).div_ceil(4)
            && self.key.2 == sps.log2_ctb_size as usize
            && self.key.3 == sps.pic_width_in_ctbs as usize
            && self.key.4 == tiles.rs_to_ts
            && self.key.5 == tiles.tile_id
    }
}

impl PicState {
    /// The per-sequence half of `PicState`: everything derived from the SPS and
    /// the tile layout, and therefore constant across the pictures that use
    /// them. Built once by [`SeqTables::get`] and shared by every picture.
    pub fn new(sps: &Sps, tiles: &TileLayout) -> Self {
        Self::with_tables(sps, &SeqTables::build(sps, tiles))
    }

    /// Re-arm an existing state for a new picture of the same geometry.
    ///
    /// Returns false if the shape changed, in which case the caller builds a
    /// fresh one. Every map is refilled with the value `new` would have given
    /// it -- this recycles the twelve allocations, not their contents. Same
    /// reasoning as `Picture::reuse`: a fresh `vec![v; n]` pays a mapping and a
    /// page fault per page the decoder later touches, where refilling warm
    /// pages is a `memset` the hardware is good at.
    pub fn reuse(&mut self, sps: &Sps, t: &SeqTables) -> bool {
        let w4 = (sps.width as usize).div_ceil(4);
        let h4 = (sps.height as usize).div_ceil(4);
        let ctb_w = sps.pic_width_in_ctbs as usize;
        let ctb_h = sps.pic_height_in_ctbs as usize;
        if (self.w4, self.h4, self.ctb_w, self.ctb_h) != (w4, h4, ctb_w, ctb_h) {
            return false;
        }
        self.width = sps.width as usize;
        self.height = sps.height as usize;
        self.log2_ctb = sps.log2_ctb_size as usize;
        self.zs = Arc::clone(&t.zs);
        self.tile_id = Arc::clone(&t.tile_id);
        self.slice_addr.fill(-1);
        self.ctb_slice.fill(0);
        self.ctb_done.fill(false);
        self.pred_mode.fill(PRED_NONE);
        // `intra_mode`, `qp_y`, `ct_depth` and `filter_bypass` are NOT reset.
        //
        // Every coding unit writes all four over its whole area -- `ct_depth`
        // and `filter_bypass` before the skip test, `qp_y` from `set_cu_qp`,
        // and `intra_mode` down all four CU shapes (skip, PCM, intra, inter) --
        // and every 4x4 of a decoded coding tree block belongs to exactly one
        // coding unit. Reads are guarded: neighbour queries go through §6.4.1
        // availability, and the loop filters skip a block whose `slice_addr` is
        // negative. So the fill was 230,400 bytes per picture of pure overwrite.
        //
        // That argument is TESTED, not asserted. Filling them with poison
        // instead -- `ct_depth` 3 (desynchronises the split_cu_flag context),
        // `filter_bypass` 1 (disables the loop filters), `intra_mode` 34 (a
        // different angular direction), `qp_y` -26 (wrong dequantisation AND
        // deblock strength) -- still decodes 147/147 with SEI 100/100. The
        // control for that probe is `nz`, which is written only where a
        // transform block has non-zero coefficients and so is genuinely not
        // covered: poisoning it gives 19/147 and SEI 12/100. The probe has
        // teeth, and these four maps are dead fills.
        //
        // The ones below stay for exactly that reason -- `nz` is sparse, and
        // `motion` is not written at all by intra coding units.
        self.edges.fill(0);
        self.nz.fill(0);
        self.motion.fill(Motion::default());
        self.sao.fill([SaoParams::default(); 3]);
        self.ctb_filter.fill(CtbFilterParams::default());
        true
    }

    /// Build a picture's state, taking the sequence-invariant tables as given.
    pub fn with_tables(sps: &Sps, t: &SeqTables) -> Self {
        let width = sps.width as usize;
        let height = sps.height as usize;
        let w4 = width.div_ceil(4);
        let h4 = height.div_ceil(4);
        let log2_ctb = sps.log2_ctb_size as usize;
        let ctb_w = sps.pic_width_in_ctbs as usize;
        let ctb_h = sps.pic_height_in_ctbs as usize;
        let n4 = w4 * h4;
        let nctb = ctb_w * ctb_h;
        let (zs, tile_id) = (Arc::clone(&t.zs), Arc::clone(&t.tile_id));
        PicState {
            width,
            height,
            w4,
            h4,
            log2_ctb,
            ctb_w,
            ctb_h,
            zs,
            slice_addr: vec![-1; nctb],
            ctb_slice: vec![0; nctb],
            ctb_done: vec![false; nctb],
            tile_id,
            pred_mode: vec![PRED_NONE; n4],
            intra_mode: vec![1; n4],
            qp_y: vec![0; n4],
            ct_depth: vec![0; n4],
            edges: vec![0; n4],
            nz: vec![0; n4],
            filter_bypass: vec![0; n4],
            motion: vec![Motion::default(); n4],
            sao: vec![[SaoParams::default(); 3]; nctb],
            ctb_filter: vec![CtbFilterParams::default(); nctb],
        }
    }

    #[inline]
    pub fn idx4(&self, x: usize, y: usize) -> usize {
        (y >> 2) * self.w4 + (x >> 2)
    }

    #[inline]
    pub fn ctb_of(&self, x: usize, y: usize) -> usize {
        (y >> self.log2_ctb) * self.ctb_w + (x >> self.log2_ctb)
    }

    /// §6.4.1 availability, split: everything about the current block.
    #[inline]
    pub fn avail_at(&self, xc: i32, yc: i32) -> AvailAt {
        let (xc, yc) = (xc as usize, yc as usize);
        let cc = self.ctb_of(xc, yc);
        // NOT `get_unchecked`. Measured: swapping this and the other hot
        // per-4x4 map reads for unchecked indexing is 0.994x / 0.992x, 10/21,
        // z = -0.22 -- a null. The per-SAMPLE loops are already `unsafe` inside
        // `rusty_h265-accel`, so what `forbid(unsafe_code)` still covers here is
        // per-BLOCK glue, run ~1.2 M times a clip against the kernels' ~700 M
        // samples. The safety boundary is already where the cost is not.
        AvailAt {
            zs: self.zs[self.idx4(xc, yc)],
            slice: self.slice_addr[cc],
            tile: self.tile_id[cc],
        }
    }

    /// [`avail_n`](Self::avail_n), returning the neighbour's 4x4 index.
    ///
    /// Every caller that finds a neighbour available then reads something at
    /// it -- `ct_depth`, `pred_mode`, `qp_y`, `intra_mode`, `motion` -- and
    /// recomputed `idx4(xn, yn)` to do so, a second multiply by the runtime
    /// stride that `avail_n` had already performed and thrown away.
    #[inline]
    pub fn avail_n_idx(&self, a: &AvailAt, xn: i32, yn: i32) -> Option<usize> {
        if xn < 0 || yn < 0 || xn >= self.width as i32 || yn >= self.height as i32 {
            return None;
        }
        let i = self.idx4(xn as usize, yn as usize);
        if self.zs[i] > a.zs {
            return None;
        }
        let cn = self.ctb_of(xn as usize, yn as usize);
        if self.slice_addr[cn] < 0 || self.slice_addr[cn] != a.slice || self.tile_id[cn] != a.tile {
            return None;
        }
        if self.pred_mode[i] == PRED_NONE {
            return None;
        }
        Some(i)
    }

    /// §6.4.1 availability, split: is (xn, yn) available to that block?
    #[inline]
    pub fn avail_n(&self, a: &AvailAt, xn: i32, yn: i32) -> bool {
        if xn < 0 || yn < 0 || xn >= self.width as i32 || yn >= self.height as i32 {
            return false;
        }
        let i = self.idx4(xn as usize, yn as usize);
        if self.zs[i] > a.zs {
            return false;
        }
        let cn = self.ctb_of(xn as usize, yn as usize);
        if self.slice_addr[cn] < 0 || self.slice_addr[cn] != a.slice || self.tile_id[cn] != a.tile {
            return false;
        }
        // Decoded at all (a lost slice leaves PRED_NONE).
        self.pred_mode[i] != PRED_NONE
    }

    /// §6.4.1: is the luma location (xn, yn) available for the block at (xc, yc)?
    #[inline]
    pub fn available(&self, xc: i32, yc: i32, xn: i32, yn: i32) -> bool {
        self.avail_n(&self.avail_at(xc, yc), xn, yn)
    }

    /// Fills a rectangle (luma sample units) of a per-4×4 map.
    pub fn fill4<T: Copy>(map: &mut [T], w4: usize, x: usize, y: usize, w: usize, h: usize, v: T) {
        let x0 = x >> 2;
        let y0 = y >> 2;
        let x1 = (x + w).div_ceil(4);
        let y1 = (y + h).div_ceil(4);
        // A row at a time, not an element at a time -- but at a CONSTANT length.
        //
        // The row slice is checked once instead of per 4x4 cell, which is why
        // this beat the element-at-a-time loop it replaced. What it also bought,
        // invisibly, was a `memset` CALL per row: `fill` on a runtime length is
        // an opaque call, and these rows are two to sixteen entries, because the
        // rectangle is a coding unit or prediction unit and the row is its width
        // over four. This runs eight to ten times per coding unit --
        // `ct_depth`, `filter_bypass`, `pred_mode`, `intra_mode`, `qp_y`,
        // `motion`, `nz` -- over the same rectangle each time, which made it the
        // densest source of tiny `memset` calls in the decoder:
        // `tools/hevc/memcpy_census.py` found NINE of them inlined into
        // `coding_quadtree` alone, all runtime-length.
        //
        // Dispatching to a fixed-size array reference gives the fill a length
        // the compiler knows, so it inlines to stores. The widths are
        // `{1,2,3,4,6,8,12,16}` -- 8x8 to 64x64 square, plus the asymmetric
        // partitions' quarter and three-quarter widths -- and anything else
        // falls through to the call, which is correct for a long row.
        macro_rules! put {
            ($row:expr, $k:literal) => {{
                if let Some(a) = $row.first_chunk_mut::<$k>() {
                    a.fill(v);
                    continue;
                }
            }};
        }
        for yy in y0..y1 {
            let r = yy * w4;
            let row = &mut map[r + x0..r + x1];
            match row.len() {
                1 => put!(row, 1),
                2 => put!(row, 2),
                3 => put!(row, 3),
                4 => put!(row, 4),
                6 => put!(row, 6),
                8 => put!(row, 8),
                12 => put!(row, 12),
                16 => put!(row, 16),
                _ => {}
            }
            row.fill(v);
        }
    }
}
