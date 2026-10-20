//! CABAC (§9.3): the arithmetic decoding engine, context initialisation and
//! the context index layout.
//!
//! The engine is the one from `rusty_h264` — HEVC kept H.264's 9-bit range
//! coder, `rangeTabLps` (Table 9-46) and `transIdx` (Table 9-47) verbatim —
//! with the fused offset/window register and the branchless decision bin.
//! What is new is the context set: 157 models, three `initType`s, and the
//! `initValue → (slope, offset)` mapping of §9.3.2.2.

use rusty_h265_accel as accel;

/// `rangeTabLps[pStateIdx][(ivlCurrRange >> 6) & 3]` (Table 9-46).
#[rustfmt::skip]
pub const RANGE_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240], [128, 167, 197, 227], [128, 158, 187, 216], [123, 150, 178, 205],
    [116, 142, 169, 195], [111, 135, 160, 185], [105, 128, 152, 175], [100, 122, 144, 166],
    [95, 116, 137, 158], [90, 110, 130, 150], [85, 104, 123, 142], [81, 99, 117, 135],
    [77, 94, 111, 128], [73, 89, 105, 122], [69, 85, 100, 116], [66, 80, 95, 110],
    [62, 76, 90, 104], [59, 72, 86, 99], [56, 69, 81, 94], [53, 65, 77, 89],
    [51, 62, 73, 85], [48, 59, 69, 80], [46, 56, 66, 76], [43, 53, 63, 72],
    [41, 50, 59, 69], [39, 48, 56, 65], [37, 45, 54, 62], [35, 43, 51, 59],
    [33, 41, 48, 56], [32, 39, 46, 53], [30, 37, 43, 50], [29, 35, 41, 48],
    [27, 33, 39, 45], [26, 31, 37, 43], [24, 30, 35, 41], [23, 28, 33, 39],
    [22, 27, 32, 37], [21, 26, 30, 35], [20, 24, 29, 33], [19, 23, 27, 31],
    [18, 22, 26, 30], [17, 21, 25, 28], [16, 20, 23, 27], [15, 19, 22, 25],
    [14, 18, 21, 24], [14, 17, 20, 23], [13, 16, 19, 22], [12, 15, 18, 21],
    [12, 14, 17, 20], [11, 14, 16, 19], [11, 13, 15, 18], [10, 12, 15, 17],
    [10, 12, 14, 16], [9, 11, 13, 15], [9, 11, 12, 14], [8, 10, 12, 14],
    [8, 9, 11, 13], [7, 9, 11, 12], [7, 9, 10, 12], [7, 8, 10, 11],
    [6, 8, 9, 11], [6, 7, 9, 10], [6, 7, 8, 9], [2, 2, 2, 2],
];

/// `[pStateIdx] -> [transIdxLps, transIdxMps]` (Table 9-47).
#[rustfmt::skip]
pub const STATE_TRANS: [[u8; 2]; 64] = [
    [0, 1], [0, 2], [1, 3], [2, 4], [2, 5], [4, 6], [4, 7], [5, 8], [6, 9], [7, 10], [8, 11], [9, 12],
    [9, 13], [11, 14], [11, 15], [12, 16], [13, 17], [13, 18], [15, 19], [15, 20], [16, 21], [16, 22],
    [18, 23], [18, 24], [19, 25], [19, 26], [21, 27], [21, 28], [22, 29], [22, 30], [23, 31], [24, 32],
    [24, 33], [25, 34], [26, 35], [26, 36], [27, 37], [27, 38], [28, 39], [29, 40], [29, 41], [30, 42],
    [30, 43], [30, 44], [31, 45], [32, 46], [32, 47], [33, 48], [33, 49], [33, 50], [34, 51], [34, 52],
    [35, 53], [35, 54], [35, 55], [36, 56], [36, 57], [36, 58], [37, 59], [37, 60], [37, 61], [38, 62],
    [38, 62], [63, 63],
];

// ---- context index layout (offsets into the model array) ----
pub const CTX_SAO_MERGE: usize = 0; // 1
pub const CTX_SAO_TYPE: usize = 1; // 1
pub const CTX_SPLIT_CU: usize = 2; // 3
pub const CTX_TRANSQUANT_BYPASS: usize = 5; // 1
pub const CTX_CU_SKIP: usize = 6; // 3
pub const CTX_CU_QP_DELTA: usize = 9; // 2
pub const CTX_PRED_MODE: usize = 11; // 1
pub const CTX_PART_MODE: usize = 12; // 4
pub const CTX_PREV_INTRA_LUMA_PRED: usize = 16; // 1
pub const CTX_INTRA_CHROMA_PRED_MODE: usize = 17; // 1
pub const CTX_MERGE_FLAG: usize = 18; // 1
pub const CTX_MERGE_IDX: usize = 19; // 1
pub const CTX_INTER_PRED_IDC: usize = 20; // 5
pub const CTX_REF_IDX: usize = 25; // 2
pub const CTX_MVD_GT0: usize = 27; // 1
pub const CTX_MVD_GT1: usize = 28; // 1
pub const CTX_MVP_FLAG: usize = 29; // 1
pub const CTX_RQT_ROOT_CBF: usize = 30; // 1
pub const CTX_SPLIT_TRANSFORM: usize = 31; // 3
pub const CTX_CBF_LUMA: usize = 34; // 2
pub const CTX_CBF_CHROMA: usize = 36; // 5 (4 + one for 4:2:2)
pub const CTX_TRANSFORM_SKIP: usize = 41; // 2 (luma, chroma)
pub const CTX_LAST_X_PREFIX: usize = 43; // 18
pub const CTX_LAST_Y_PREFIX: usize = 61; // 18
pub const CTX_CSBF: usize = 79; // 4 (luma 2, chroma 2)
pub const CTX_SIG: usize = 83; // 42 (luma 27, chroma 15)
pub const CTX_SIG_TS: usize = 125; // 2 (RExt transform-skip contexts, unused in v1)
pub const CTX_GT1: usize = 127; // 24 (luma 16, chroma 8)
pub const CTX_GT2: usize = 151; // 6 (luma 4, chroma 2)
pub const NUM_CTX: usize = 157;
/// The context array's allocated length: `NUM_CTX` rounded up to a power of two.
///
/// The guard on `decode`'s index used to be `ctx_idx.min(NUM_CTX - 1)`, which
/// emitted `cmp` + `mov imm` + `cmov` -- three instructions per context-coded
/// bin, on the dependency chain that feeds the model load. Every call site is a
/// constant base plus a bounded offset, so the guard has never actually clamped
/// anything; it is there to prove the index in range. Padding the array to 256
/// proves it with a single `and`, with no panic edge and no `unsafe`. The 99
/// pad bytes are never read or written.
pub const CTX_PAD: usize = 256;

/// `initValue` per context for `initType` 0 (I), 1, 2 — Tables 9-5 … 9-37,
/// transcribed from HM `ContextTables.h` (BSD-3), whose arrays are ordered
/// [B, P, I] = initType [2, 1, 0].
const CNU: u8 = 154;
#[rustfmt::skip]
pub static INIT_VALUES: [[u8; NUM_CTX]; 3] = [
    // initType 0 — I slices
    [
        153, // sao_merge
        200, // sao_type_idx
        139, 141, 157, // split_cu_flag
        154, // cu_transquant_bypass_flag
        CNU, CNU, CNU, // cu_skip_flag
        154, 154, // cu_qp_delta_abs
        CNU, // pred_mode_flag
        184, CNU, CNU, CNU, // part_mode
        184, // prev_intra_luma_pred_flag
        63, // intra_chroma_pred_mode
        CNU, // merge_flag
        CNU, // merge_idx
        CNU, CNU, CNU, CNU, CNU, // inter_pred_idc
        CNU, CNU, // ref_idx
        CNU, // abs_mvd_greater0
        CNU, // abs_mvd_greater1
        CNU, // mvp_lx_flag
        CNU, // rqt_root_cbf
        153, 138, 138, // split_transform_flag
        111, 141, // cbf_luma
        94, 138, 182, 154, 154, // cbf_cb / cbf_cr
        139, 139, // transform_skip_flag
        110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, // last_x
        110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, // last_y
        91, 171, 134, 141, // coded_sub_block_flag
        111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, // sig luma
        140, 139, 182, 182, 152, 136, 152, 136, 153, 136, 139, 111, 136, 139, 111, // sig chroma
        141, 111, // sig transform-skip (RExt)
        140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122, 152, // gt1 luma
        140, 179, 166, 182, 140, 227, 122, 197, // gt1 chroma
        138, 153, 136, 167, 152, 152, // gt2
    ],
    // initType 1
    [
        153,
        185,
        107, 139, 126,
        154,
        197, 185, 201,
        154, 154,
        149,
        154, 139, 154, 154,
        154,
        152,
        110,
        122,
        95, 79, 63, 31, 31,
        153, 153,
        140,
        198,
        168,
        79,
        124, 138, 94,
        153, 111,
        149, 107, 167, 154, 154,
        139, 139,
        125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110, 111, 111, 95, 94, 108, 123, 108,
        125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110, 111, 111, 95, 94, 108, 123, 108,
        121, 140, 61, 154,
        155, 154, 139, 153, 139, 123, 123, 63, 153, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154,
        170, 153, 123, 123, 107, 121, 107, 121, 167, 151, 183, 140, 151, 183, 140,
        140, 140,
        154, 196, 196, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 137,
        169, 194, 166, 167, 154, 167, 137, 182,
        107, 167, 91, 122, 107, 167,
    ],
    // initType 2
    [
        153,
        160,
        107, 139, 126,
        154,
        197, 185, 201,
        154, 154,
        134,
        154, 139, 154, 154,
        183,
        152,
        154,
        137,
        95, 79, 63, 31, 31,
        153, 153,
        169,
        198,
        168,
        79,
        224, 167, 122,
        153, 111,
        149, 92, 167, 154, 154,
        139, 139,
        125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125, 126, 111, 111, 79, 108, 123, 93,
        125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125, 126, 111, 111, 79, 108, 123, 93,
        121, 140, 61, 154,
        170, 154, 139, 153, 139, 123, 123, 63, 124, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154,
        170, 153, 138, 138, 122, 121, 122, 121, 167, 151, 183, 140, 151, 183, 140,
        140, 140,
        154, 196, 167, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 122,
        169, 208, 166, 167, 154, 152, 167, 182,
        107, 167, 91, 107, 107, 167,
    ],
];

/// Fused per-(packed-state, quartile) record: `lps | transMps<<8 | transLps<<16`.
/// A model is one byte `pStateIdx * 2 + valMps`; the state-0 MPS flip is folded in.
///
/// The layout is STATE-major and spans the whole 0..=255 byte domain, both
/// deliberately — `decode` is 89% of all bins, so this table's shape is priced
/// per bin:
///
///   * state-major puts a context's four quartile records in one aligned
///     16-byte group, so the line a bin touches serves that context whatever
///     `ivlCurrRange` happens to be. Quartile-major put them 512 B apart —
///     four lines per context, three of them cold.
///   * indexing the full byte retires the `& 127`. A model byte is
///     `pStateIdx * 2 + valMps` with `pStateIdx < 64`, so it never exceeds 127
///     and the mask was already a no-op on the value; it was there to prove the
///     index in range. Mirroring the table into 128..=255 proves it instead,
///     for free, and the mask leaves the per-bin path.
const fn build_fused() -> [u32; 256 * 4] {
    let mut t = [0u32; 256 * 4];
    let mut s = 0;
    while s < 256 {
        let p = (s & 127) >> 1;
        let mut q = 0;
        while q < 4 {
            let lps = RANGE_LPS[p][q] as u32;
            let mps = s as u8 & 1;
            let tm = ((STATE_TRANS[p][1] << 1) | mps) as u32;
            let new_mps = if p == 0 { 1 - mps } else { mps };
            let tl = ((STATE_TRANS[p][0] << 1) | new_mps) as u32;
            t[s * 4 + q] = lps | (tm << 8) | (tl << 16);
            q += 1;
        }
        s += 1;
    }
    t
}
static FUSED: [u32; 256 * 4] = build_fused();

/// Bit position of the arithmetic offset inside [`Cabac::low`].
const OFF: u32 = 41;
const REFILL_AT: i32 = 8;

/// The context models alone (for WPP storage/sync, §9.3.2.3 / §9.3.2.4).
#[derive(Clone)]
pub struct Contexts(pub [u8; CTX_PAD]);

impl Contexts {
    /// §9.3.2.2: initialise every model for `init_type` (0/1/2) and `SliceQpY`.
    pub fn init(init_type: usize, slice_qp: i32) -> Self {
        let q = slice_qp.clamp(0, 51);
        let mut ctx = [0u8; CTX_PAD];
        for (i, c) in ctx.iter_mut().enumerate().take(NUM_CTX) {
            let init_value = INIT_VALUES[init_type][i] as i32;
            let slope_idx = init_value >> 4;
            let offset_idx = init_value & 15;
            let m = slope_idx * 5 - 45;
            let n = (offset_idx << 3) - 16;
            let pre = (((m * q) >> 4) + n).clamp(1, 126);
            *c = if pre <= 63 { ((63 - pre) as u8) << 1 } else { (((pre - 64) as u8) << 1) | 1 };
        }
        Contexts(ctx)
    }
}

/// The arithmetic decoder over one slice segment's RBSP.
pub struct Cabac<'a> {
    data: &'a [u8],
    byte_pos: usize,
    /// `low = ivlOffset · 2^41 + buffered bits (< 2^41)`.
    low: u64,
    cnt: i32,
    range: u32,
    pub ctx: Contexts,
    #[cfg(feature = "cabac-trace")]
    trace: bool,
    #[cfg(feature = "cabac-trace")]
    sym: u64,
}

impl<'a> Cabac<'a> {
    /// Starts the engine at byte `start` of `data` (§9.3.2.5) with `ctx`.
    pub fn new(data: &'a [u8], start: usize, ctx: Contexts) -> Self {
        let mut e = Cabac {
            data,
            byte_pos: start,
            low: 0,
            cnt: 0,
            range: 510,
            ctx,
            #[cfg(feature = "cabac-trace")]
            trace: crate::knob("RH265_CABAC_TRACE"),
            #[cfg(feature = "cabac-trace")]
            sym: 0,
        };
        e.reinit_at(start);
        e
    }

    /// Re-initialises the arithmetic registers at absolute byte `byte`,
    /// keeping the context models (after PCM samples, §9.3.2.6).
    pub fn reinit_at(&mut self, byte: usize) {
        self.byte_pos = byte;
        self.low = 0;
        self.cnt = 0;
        self.range = 510;
        self.refill();
        self.low <<= 9;
        self.cnt -= 9;
    }

    /// The byte at which byte-aligned raw data (PCM samples) begins after a
    /// terminate bin decoded as 1: the consumed bit count rounded up.
    pub fn aligned_byte_pos(&self) -> usize {
        let consumed = self.byte_pos as isize * 8 - self.cnt as isize;
        ((consumed + 7) >> 3) as usize
    }

    /// Bytes read so far (an upper bound on the engine's position).
    pub fn byte_pos(&self) -> usize {
        self.byte_pos
    }

    /// The last four bytes, one at a time, for the handful of refills that run
    /// off the end of the slice segment.
    ///
    /// `#[cold]` and out of line on purpose. `decode` is `#[inline(always)]`
    /// and is inlined at ~50 call sites; with this arm inline, each of those
    /// carried ~55 instructions of byte-at-a-time bounds-checked fallback that
    /// runs a few times per slice. It never affected the hot path's speed, only
    /// how much instruction cache the hot path was spread across.
    #[cold]
    #[inline(never)]
    fn refill_tail(&self) -> u32 {
        let b = |i: usize| self.data.get(self.byte_pos + i).copied().unwrap_or(0) as u32;
        (b(0) << 24) | (b(1) << 16) | (b(2) << 8) | b(3)
    }

    #[inline]
    fn refill(&mut self) {
        let v = match self.data.get(self.byte_pos..self.byte_pos + 4) {
            Some(c) => u32::from_be_bytes([c[0], c[1], c[2], c[3]]),
            None => self.refill_tail(),
        };
        self.low |= (v as u64) << ((OFF as i32 - 32 - self.cnt) as u32);
        self.byte_pos += 4;
        self.cnt += 32;
    }

    #[inline(always)]
    fn renorm(&mut self) {
        let n = self.range.leading_zeros() - 23;
        self.range <<= n;
        self.low <<= n;
        self.cnt -= n as i32;
        if self.cnt < REFILL_AT {
            self.refill();
        }
    }

    /// The bin trace, behind `cabac-trace` (off by default).
    ///
    /// This used to be a plain `if self.trace` on the per-bin path: a load, a
    /// test and a branch on every one of the ~4 M context bins and ~0.5 M
    /// bypass bins of a 720p stream, serving a debugging facility no shipping
    /// decode reads. As a `cfg` it is exactly as useful and costs nothing.
    ///
    /// `low` is a parameter because the bypass runs below hold it in a
    /// register rather than in the struct.
    #[inline(always)]
    #[allow(unused_variables)]
    fn tr(&mut self, kind: &str, low: u64) {
        #[cfg(feature = "cabac-trace")]
        if self.trace {
            eprintln!("{} {} r={} o={}", self.sym, kind, self.range, low >> OFF);
            self.sym += 1;
        }
    }

    /// Context-coded bin (§9.3.4.3.2).
    #[inline(always)]
    pub fn decode(&mut self, ctx_idx: usize) -> u32 {
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::CABAC_CTX_BINS, 1);
        }
        self.tr("D", self.low);
        // One `and`, not `cmp`/`mov`/`cmov` -- see `CTX_PAD`.
        let ctx_idx = ctx_idx & (CTX_PAD - 1);
        let s = self.ctx.0[ctx_idx] as usize;
        let q = ((self.range >> 6) & 3) as usize;
        // `s < 256` and `q < 4`, so this index is in range by construction:
        // no mask on the value, no bounds check. See `build_fused`.
        let e = FUSED[(s << 2) | q];
        let lps = e & 0xFF;
        debug_assert!(self.range >= 256);
        self.range -= lps;
        let scaled = (self.range as u64) << OFF;
        let mask64 = ((scaled as i64 - self.low as i64 - 1) >> 63) as u64;
        let mask = mask64 as u32;
        self.low -= scaled & mask64;
        self.range = self.range.wrapping_add(lps.wrapping_sub(self.range) & mask);
        self.ctx.0[ctx_idx] = ((e >> (8 + (mask & 8))) & 0xFF) as u8;
        let bin = (s as u32 ^ mask) & 1;
        self.renorm();
        bin
    }

    /// The compare-subtract half of a bypass bin, branchless.
    ///
    /// A bypass bin is an equiprobable coin flip, so the `if low >= scaled`
    /// this replaces mispredicted about half the times it ran — 0.5 M bins on a
    /// 720p stream, 15 M on intra-heavy content. Five dependent ALU ops beat a
    /// coin-flip branch by a wide margin.
    ///
    /// `d = low - scaled` wraps negative exactly when the bin is 0; both
    /// operands are below `2^51`, so bit 63 of the wrapped difference is the
    /// sign and `m` is all-ones on 0, zero on 1. Adding `scaled & m` back
    /// restores `low` in the 0 case.
    #[inline(always)]
    fn bypass_cmp(low: u64, scaled: u64) -> (u64, u32) {
        let d = low.wrapping_sub(scaled);
        let m = ((d as i64) >> 63) as u64;
        (d.wrapping_add(scaled & m), (!m as u32) & 1)
    }

    /// Bypass bin (§9.3.4.3.4).
    #[inline(always)]
    pub fn bypass(&mut self) -> u32 {
        self.tr("B", self.low);
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::CABAC_BYPASS_BINS, 1);
        }
        self.low <<= 1;
        self.cnt -= 1;
        if self.cnt < REFILL_AT {
            self.refill();
        }
        let (low, bin) = Self::bypass_cmp(self.low, (self.range as u64) << OFF);
        self.low = low;
        bin
    }

    /// `n` bypass bins, MSB first (fixed-length binarisation).
    ///
    /// `ivlCurrRange` is invariant across bypass bins, so `scaled` is a
    /// loop-invariant 64-bit shift that the old per-bin `bypass()` recomputed
    /// every time; `low` and `cnt` likewise stay in registers for the run
    /// instead of round-tripping through the struct once per bin.
    #[inline]
    pub fn bypass_bits(&mut self, n: u32) -> u32 {
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::CABAC_BYPASS_CALLS, 1);
            accel::census::bump(&accel::census::CABAC_BYPASS_BINS, n as u64);
        }
        // The census reads 1.86 bins per call: `rice` and the last-position
        // suffix width are 0 often enough that skipping the run setup pays.
        if n == 0 {
            return 0;
        }
        let scaled = (self.range as u64) << OFF;
        let (mut low, mut cnt) = (self.low, self.cnt);
        let mut v = 0u32;
        for _ in 0..n {
            self.tr("B", low);
            low <<= 1;
            cnt -= 1;
            if cnt < REFILL_AT {
                self.low = low;
                self.cnt = cnt;
                self.refill();
                low = self.low;
                cnt = self.cnt;
            }
            let (l, bin) = Self::bypass_cmp(low, scaled);
            low = l;
            v = (v << 1) | bin;
        }
        self.low = low;
        self.cnt = cnt;
        v
    }

    /// Bypass bins until one decodes 0, or until `max` of them decode 1;
    /// returns how many 1s. This is the unary prefix of
    /// `coeff_abs_level_remaining` (§9.3.3.11) and of EGk, which between them
    /// are most of the bypass population on coefficient-heavy content.
    ///
    /// Same loop-invariant treatment as [`bypass_bits`]. The compare stays a
    /// branch here because it *is* the loop exit; unlike a fixed-length suffix
    /// it is strongly biased toward falling out early.
    #[inline]
    pub fn bypass_ones(&mut self, max: u32) -> u32 {
        let scaled = (self.range as u64) << OFF;
        let (mut low, mut cnt) = (self.low, self.cnt);
        let mut k = 0;
        while k < max {
            self.tr("B", low);
            low <<= 1;
            cnt -= 1;
            if cnt < REFILL_AT {
                self.low = low;
                self.cnt = cnt;
                self.refill();
                low = self.low;
                cnt = self.cnt;
            }
            if low < scaled {
                break;
            }
            low -= scaled;
            k += 1;
        }
        self.low = low;
        self.cnt = cnt;
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::CABAC_BYPASS_CALLS, 1);
            accel::census::bump(&accel::census::CABAC_BYPASS_BINS, (k + 1).min(max) as u64);
        }
        k
    }

    /// Terminate bin (§9.3.4.3.5). `true` = end of slice segment / substream / PCM.
    #[inline(always)]
    pub fn terminate(&mut self) -> bool {
        if accel::census::ALWAYS {
            accel::census::bump(&accel::census::CABAC_TERM_BINS, 1);
        }
        self.tr("T", self.low);
        self.range -= 2;
        if self.low >= (self.range as u64) << OFF {
            true
        } else {
            self.renorm();
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_matches_spec_examples() {
        // initValue 154 → m = 0, n = 64 → pre = 64 → state 0, mps 1
        let c = Contexts::init(0, 26);
        assert_eq!(c.0[CTX_TRANSQUANT_BYPASS], 1);
        // initValue 139 (split_cu I ctx0): slope 8 → m = -5; offset 11 → n = 72;
        // qp 26: pre = ((-5*26)>>4) + 72 = -9 + 72 = 63 → state 0, mps 0
        assert_eq!(c.0[CTX_SPLIT_CU], 0);
        // initValue 200: slope 12 → m = 15, offset 8 → n = 48; qp 26 → (390>>4)=24 → 72 → state 8 mps 1
        assert_eq!(c.0[CTX_SAO_TYPE], (8 << 1) | 1);
    }

    #[test]
    fn table_sizes() {
        for t in INIT_VALUES.iter() {
            assert_eq!(t.len(), NUM_CTX);
        }
        assert_eq!(RANGE_LPS[63], [2, 2, 2, 2]);
        assert_eq!(STATE_TRANS[63], [63, 63]);
    }

    #[test]
    fn engine_decodes_terminate_on_flushed_stream() {
        let data = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut e = Cabac::new(&data, 0, Contexts::init(0, 30));
        assert!(e.terminate());
        assert_eq!(e.aligned_byte_pos(), 2);
    }
}
