//! Spec constant tables, built at compile time and validated by tests:
//! scan orders (§6.5.3–6.5.5), the 32×32 DCT matrix (§8.6.4.2), the 4×4 DST,
//! intra angles (Tables 8-4/8-5), the chroma QP map (Table 8-10), the
//! dequantisation scale (§8.6.3) and the deblocking β/tC tables (Table 8-12).

/// Up-right diagonal scan (§6.5.3) for a `SIZE`×`SIZE` block, as (x, y).
const fn diag_scan<const SIZE: usize, const N: usize>() -> [(u8, u8); N] {
    let mut out = [(0u8, 0u8); N];
    let mut i = 0;
    let mut x: i32 = 0;
    let mut y: i32 = 0;
    let mut stop = false;
    while !stop {
        while y >= 0 {
            if x < SIZE as i32 && y < SIZE as i32 {
                out[i] = (x as u8, y as u8);
                i += 1;
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
        if i >= N {
            stop = true;
        }
    }
    out
}

const fn horiz_scan<const SIZE: usize, const N: usize>() -> [(u8, u8); N] {
    let mut out = [(0u8, 0u8); N];
    let mut i = 0;
    while i < N {
        out[i] = ((i % SIZE) as u8, (i / SIZE) as u8);
        i += 1;
    }
    out
}

const fn vert_scan<const SIZE: usize, const N: usize>() -> [(u8, u8); N] {
    let mut out = [(0u8, 0u8); N];
    let mut i = 0;
    while i < N {
        out[i] = ((i / SIZE) as u8, (i % SIZE) as u8);
        i += 1;
    }
    out
}

pub static DIAG_2X2: [(u8, u8); 4] = diag_scan::<2, 4>();
pub static DIAG_4X4: [(u8, u8); 16] = diag_scan::<4, 16>();
pub static DIAG_8X8: [(u8, u8); 64] = diag_scan::<8, 64>();
pub static HORIZ_2X2: [(u8, u8); 4] = horiz_scan::<2, 4>();
pub static HORIZ_4X4: [(u8, u8); 16] = horiz_scan::<4, 16>();
pub static HORIZ_8X8: [(u8, u8); 64] = horiz_scan::<8, 64>();
pub static VERT_2X2: [(u8, u8); 4] = vert_scan::<2, 4>();
pub static VERT_4X4: [(u8, u8); 16] = vert_scan::<4, 16>();
pub static VERT_8X8: [(u8, u8); 64] = vert_scan::<8, 64>();
static ONE: [(u8, u8); 1] = [(0, 0)];

/// The inverse of a scan: `t[y * SIZE + x]` is the scan position of (x, y).
const fn invert_scan<const SIZE: usize, const N: usize>(f: [(u8, u8); N]) -> [u8; N] {
    let mut t = [0u8; N];
    let mut i = 0;
    while i < N {
        let (x, y) = f[i];
        t[(y as usize) * SIZE + x as usize] = i as u8;
        i += 1;
    }
    t
}

/// Running bounding box of a scan prefix: `t[k]` is `(max_x + 1, max_y + 1)`
/// over scan positions `0..=k`.
const fn bbox_scan<const N: usize>(f: [(u8, u8); N]) -> [(u8, u8); N] {
    let mut t = [(0u8, 0u8); N];
    let (mut w, mut h) = (0u8, 0u8);
    let mut i = 0;
    while i < N {
        let (x, y) = f[i];
        if x + 1 > w {
            w = x + 1;
        }
        if y + 1 > h {
            h = y + 1;
        }
        t[i] = (w, h);
        i += 1;
    }
    t
}

static INV_DIAG_2X2: [u8; 4] = invert_scan::<2, 4>(diag_scan::<2, 4>());
static INV_DIAG_4X4: [u8; 16] = invert_scan::<4, 16>(diag_scan::<4, 16>());
static INV_DIAG_8X8: [u8; 64] = invert_scan::<8, 64>(diag_scan::<8, 64>());
static INV_HORIZ_2X2: [u8; 4] = invert_scan::<2, 4>(horiz_scan::<2, 4>());
static INV_HORIZ_4X4: [u8; 16] = invert_scan::<4, 16>(horiz_scan::<4, 16>());
static INV_HORIZ_8X8: [u8; 64] = invert_scan::<8, 64>(horiz_scan::<8, 64>());
static INV_VERT_2X2: [u8; 4] = invert_scan::<2, 4>(vert_scan::<2, 4>());
static INV_VERT_4X4: [u8; 16] = invert_scan::<4, 16>(vert_scan::<4, 16>());
static INV_VERT_8X8: [u8; 64] = invert_scan::<8, 64>(vert_scan::<8, 64>());
static INV_ONE: [u8; 1] = [0];

static BBOX_DIAG_2X2: [(u8, u8); 4] = bbox_scan(diag_scan::<2, 4>());
static BBOX_DIAG_4X4: [(u8, u8); 16] = bbox_scan(diag_scan::<4, 16>());
static BBOX_DIAG_8X8: [(u8, u8); 64] = bbox_scan(diag_scan::<8, 64>());
static BBOX_HORIZ_2X2: [(u8, u8); 4] = bbox_scan(horiz_scan::<2, 4>());
static BBOX_HORIZ_4X4: [(u8, u8); 16] = bbox_scan(horiz_scan::<4, 16>());
static BBOX_HORIZ_8X8: [(u8, u8); 64] = bbox_scan(horiz_scan::<8, 64>());
static BBOX_VERT_2X2: [(u8, u8); 4] = bbox_scan(vert_scan::<2, 4>());
static BBOX_VERT_4X4: [(u8, u8); 16] = bbox_scan(vert_scan::<4, 16>());
static BBOX_VERT_8X8: [(u8, u8); 64] = bbox_scan(vert_scan::<8, 64>());
static BBOX_ONE: [(u8, u8); 1] = [(1, 1)];

/// `ScanOrder[log2BlockSize][scanIdx]` for block sizes 1, 2, 4, 8
/// (scanIdx 0 = diagonal, 1 = horizontal, 2 = vertical).
pub fn scan_order(log2_block_size: usize, scan_idx: usize) -> &'static [(u8, u8)] {
    match (log2_block_size, scan_idx) {
        (0, _) => &ONE,
        (1, 0) => &DIAG_2X2,
        (1, 1) => &HORIZ_2X2,
        (1, _) => &VERT_2X2,
        (2, 0) => &DIAG_4X4,
        (2, 1) => &HORIZ_4X4,
        (2, _) => &VERT_4X4,
        (_, 0) => &DIAG_8X8,
        (_, 1) => &HORIZ_8X8,
        (_, _) => &VERT_8X8,
    }
}

/// The inverse of [`scan_order`]: `t[y * size + x]` is the scan position of
/// (x, y).
///
/// The residual parser needs exactly this, twice per transform block, to turn
/// the decoded last-significant coordinate into a scan index. It used to search
/// the forward table linearly — 13.5 tuple compares per block on a 720p stream
/// and 5.0 per block over 3.0 M blocks on intra-heavy content, for a lookup
/// that is one load.
pub fn scan_inverse(log2_block_size: usize, scan_idx: usize) -> &'static [u8] {
    match (log2_block_size, scan_idx) {
        (0, _) => &INV_ONE,
        (1, 0) => &INV_DIAG_2X2,
        (1, 1) => &INV_HORIZ_2X2,
        (1, _) => &INV_VERT_2X2,
        (2, 0) => &INV_DIAG_4X4,
        (2, 1) => &INV_HORIZ_4X4,
        (2, _) => &INV_VERT_4X4,
        (_, 0) => &INV_DIAG_8X8,
        (_, 1) => &INV_HORIZ_8X8,
        (_, _) => &INV_VERT_8X8,
    }
}

/// `t[k] = (max_x + 1, max_y + 1)` over scan positions `0..=k` of
/// [`scan_order`] — the bounding box of a scan prefix.
///
/// With `k = lastSubBlock` this is exactly the region of a transform block that
/// can still receive a coefficient, which is how much of it needs clearing.
pub fn scan_bbox(log2_block_size: usize, scan_idx: usize) -> &'static [(u8, u8)] {
    match (log2_block_size, scan_idx) {
        (0, _) => &BBOX_ONE,
        (1, 0) => &BBOX_DIAG_2X2,
        (1, 1) => &BBOX_HORIZ_2X2,
        (1, _) => &BBOX_VERT_2X2,
        (2, 0) => &BBOX_DIAG_4X4,
        (2, 1) => &BBOX_HORIZ_4X4,
        (2, _) => &BBOX_VERT_4X4,
        (_, 0) => &BBOX_DIAG_8X8,
        (_, 1) => &BBOX_HORIZ_8X8,
        (_, _) => &BBOX_VERT_8X8,
    }
}

/// Everything the residual parser needs about one (sub-block size, scanIdx)
/// pair, selected once instead of five times.
///
/// `residual_block` used to call `scan_order` twice, `scan_inverse` twice and
/// `scan_bbox` once -- five independent `match`es on the same two values, each
/// emitting its own chain of compares and `cmov`s with a `lea` per candidate
/// table. Three of those five were added by this campaign, so the per-block
/// dispatch cost grew while the per-coefficient loops shrank. One indexed
/// lookup replaces all five.
pub struct ScanSet {
    /// Sub-block scan, and its inverse and prefix bounding box.
    pub sb: &'static [(u8, u8)],
    pub sb_inv: &'static [u8],
    pub sb_bbox: &'static [(u8, u8)],
    /// The 4x4 within-sub-block scan and its inverse. Fixed-size on purpose:
    /// a sub-block is always 4x4, so the length belongs in the type, and the
    /// bounds check on `pos[n]` goes away for every scanned position.
    pub pos: &'static [(u8, u8); 16],
    pub pos_inv: &'static [u8; 16],
    /// The significance-context rows for this scan (§9.3.4.2.5): the 4x4-block
    /// map, and the neighbour term indexed by `prevCsbf`. Both were separate
    /// `[..][scan_idx]` lookups in the per-sub-block loop, each re-proving a
    /// bound the `ScanSet` selection has already settled.
    pub sig_4x4: &'static [u8; 16],
    pub sig_nb: &'static [[u8; 16]; 4],
}

macro_rules! scan_set {
    ($sb:ident, $inv:ident, $bb:ident, $pos:ident, $pinv:ident, $si:expr) => {
        ScanSet {
            sb: &$sb,
            sb_inv: &$inv,
            sb_bbox: &$bb,
            pos: &$pos,
            pos_inv: &$pinv,
            sig_4x4: &SIG_CTX_4X4_BY_SCAN[$si],
            sig_nb: &SIG_NB[$si],
        }
    };
}

/// `SCAN_SETS[log2SubBlockSize][scanIdx]`.
static SCAN_SETS: [[ScanSet; 3]; 4] = [
    [
        scan_set!(ONE, INV_ONE, BBOX_ONE, DIAG_4X4, INV_DIAG_4X4, 0),
        scan_set!(ONE, INV_ONE, BBOX_ONE, HORIZ_4X4, INV_HORIZ_4X4, 1),
        scan_set!(ONE, INV_ONE, BBOX_ONE, VERT_4X4, INV_VERT_4X4, 2),
    ],
    [
        scan_set!(DIAG_2X2, INV_DIAG_2X2, BBOX_DIAG_2X2, DIAG_4X4, INV_DIAG_4X4, 0),
        scan_set!(HORIZ_2X2, INV_HORIZ_2X2, BBOX_HORIZ_2X2, HORIZ_4X4, INV_HORIZ_4X4, 1),
        scan_set!(VERT_2X2, INV_VERT_2X2, BBOX_VERT_2X2, VERT_4X4, INV_VERT_4X4, 2),
    ],
    [
        scan_set!(DIAG_4X4, INV_DIAG_4X4, BBOX_DIAG_4X4, DIAG_4X4, INV_DIAG_4X4, 0),
        scan_set!(HORIZ_4X4, INV_HORIZ_4X4, BBOX_HORIZ_4X4, HORIZ_4X4, INV_HORIZ_4X4, 1),
        scan_set!(VERT_4X4, INV_VERT_4X4, BBOX_VERT_4X4, VERT_4X4, INV_VERT_4X4, 2),
    ],
    [
        scan_set!(DIAG_8X8, INV_DIAG_8X8, BBOX_DIAG_8X8, DIAG_4X4, INV_DIAG_4X4, 0),
        scan_set!(HORIZ_8X8, INV_HORIZ_8X8, BBOX_HORIZ_8X8, HORIZ_4X4, INV_HORIZ_4X4, 1),
        scan_set!(VERT_8X8, INV_VERT_8X8, BBOX_VERT_8X8, VERT_4X4, INV_VERT_4X4, 2),
    ],
];

/// The scan tables for one transform block, in one lookup.
///
/// `log2sb` is `log2TrafoSize - 2` and so is 0..=3; `scan_idx` is 0..=2.
#[inline]
pub fn scan_set(log2sb: usize, scan_idx: usize) -> &'static ScanSet {
    &SCAN_SETS[log2sb.min(3)][scan_idx.min(2)]
}

/// `transMatrix` column 0 = the unique DCT coefficient magnitudes at angles
/// m·π/64 for m = 0..=32 (§8.6.4.2). Every other entry follows from the
/// cosine symmetries, which is how [`DCT32`] is generated.
const DCT_BASE: [i16; 33] = [
    64, 90, 90, 90, 89, 88, 87, 85, 83, 82, 80, 78, 75, 73, 70, 67, 64, 61, 57, 54, 50, 46, 43, 38, 36, 31, 25, 22, 18, 13, 9, 4, 0,
];

const fn build_dct32() -> [[i16; 32]; 32] {
    let mut m = [[0i16; 32]; 32];
    let mut k = 0;
    while k < 32 {
        let mut n = 0;
        while n < 32 {
            let a = ((2 * n + 1) * k) % 128;
            m[k][n] = if k == 0 {
                64
            } else if a <= 32 {
                DCT_BASE[a]
            } else if a <= 64 {
                -DCT_BASE[64 - a]
            } else if a <= 96 {
                -DCT_BASE[a - 64]
            } else {
                DCT_BASE[128 - a]
            };
            n += 1;
        }
        k += 1;
    }
    m
}

/// The 32×32 inverse-DCT matrix `transMatrix[k][n]`; the N-point matrix is
/// rows `k · 32/N`, columns `0..N`.
pub static DCT32: [[i16; 32]; 32] = build_dct32();

/// 4×4 DST-VII for intra luma 4×4 blocks (§8.6.4.2).
pub static DST4: [[i16; 4]; 4] = [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];

/// [`DST4`] padded to 32-wide rows.
///
/// The inverse-transform kernel indexes `tab[k * tstep * 32]`, a pitch the DCT
/// table has and the DST does not. Padding costs 224 bytes of `.rodata` and
/// lets the 4-point DST use the same accumulate as everything else instead of
/// keeping a scalar loop of its own. `dst4_padding_matches_dst4` pins the two
/// together so an edit to one cannot drift from the other.
pub static DST4_PAD: [[i16; 32]; 4] = {
    let mut t = [[0i16; 32]; 4];
    let mut k = 0;
    while k < 4 {
        let mut j = 0;
        while j < 4 {
            t[k][j] = DST4[k][j];
            j += 1;
        }
        k += 1;
    }
    t
};

/// `intraPredAngle` by mode 2..=34 (Table 8-4); modes 0/1 are unused.
pub static INTRA_PRED_ANGLE: [i32; 35] = [
    0, 0, 32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32,
];

/// `invAngle` by mode 11..=25 (Table 8-5), indexed by `mode - 11`.
pub static INV_ANGLE: [i32; 15] = [-4096, -1638, -910, -630, -482, -390, -315, -256, -315, -390, -482, -630, -910, -1638, -4096];

/// `QpC` as a function of `qPi` for ChromaArrayType 1 (Table 8-10), qPi 0..=57.
pub static CHROMA_QP_420: [u8; 58] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37, 38, 39, 40, 41, 42, 43, 44,
    45, 46, 47, 48, 49, 50, 51,
];

/// `levelScale[qP % 6]` (§8.6.3).
pub static LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// Deblocking `β′` by Q (Table 8-12), Q 0..=51.
pub static BETA_TABLE: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64,
];

/// Deblocking `tC′` by Q (Table 8-12), Q 0..=53.
pub static TC_TABLE: [u8; 54] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24,
];

/// `ctxIdxMap` for `sig_coeff_flag` in 4×4 blocks (§9.3.4.2.5), index `(yC << 2) + xC`.
pub static SIG_CTX_MAP_4X4: [u8; 16] = SIG_MAP_4X4;
const SIG_MAP_4X4: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

/// [`SIG_CTX_MAP_4X4`] re-keyed by SCAN POSITION rather than by (x, y):
/// `SIG_CTX_4X4_BY_SCAN[scanIdx][n]`.
///
/// The significance loop walks `n` downward and needs nothing else from the
/// position — so keying the map by `n` retires the two table loads and four
/// adds/shifts that recovered (xC, yC) for every scanned position, 2.3 M of
/// them on a 720p stream and 18.8 M on intra-heavy content.
const fn sig_map_by_scan(f: [(u8, u8); 16]) -> [u8; 16] {
    let mut t = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        let (x, y) = f[i];
        t[i] = SIG_MAP_4X4[(y as usize) * 4 + x as usize];
        i += 1;
    }
    t
}
pub static SIG_CTX_4X4_BY_SCAN: [[u8; 16]; 3] = [sig_map_by_scan(diag_scan::<4, 16>()), sig_map_by_scan(horiz_scan::<4, 16>()), sig_map_by_scan(vert_scan::<4, 16>())];

/// `SIG_NB[scanIdx][prevCsbf][n]`: the 0/1/2 neighbour term of §9.3.4.2.5 for
/// scan position `n` inside a 4x4 sub-block of a block larger than 4x4.
///
/// The spec writes this as a decision on `prevCsbf` and then on (xP, yP); as a
/// nest of compares it ran up to four branches per scanned position. The whole
/// thing is 192 bytes of constant, so it is one load.
const fn sig_nb_one(f: [(u8, u8); 16], prev: usize) -> [u8; 16] {
    let mut t = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        let (xp, yp) = (f[i].0 as usize, f[i].1 as usize);
        t[i] = match prev {
            0 => {
                if xp + yp == 0 {
                    2
                } else if xp + yp < 3 {
                    1
                } else {
                    0
                }
            }
            1 => {
                if yp == 0 {
                    2
                } else if yp == 1 {
                    1
                } else {
                    0
                }
            }
            2 => {
                if xp == 0 {
                    2
                } else if xp == 1 {
                    1
                } else {
                    0
                }
            }
            _ => 2,
        };
        i += 1;
    }
    t
}
const fn sig_nb_set(f: [(u8, u8); 16]) -> [[u8; 16]; 4] {
    [sig_nb_one(f, 0), sig_nb_one(f, 1), sig_nb_one(f, 2), sig_nb_one(f, 3)]
}
pub static SIG_NB: [[[u8; 16]; 4]; 3] = [sig_nb_set(diag_scan::<4, 16>()), sig_nb_set(horiz_scan::<4, 16>()), sig_nb_set(vert_scan::<4, 16>())];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diag_scan_matches_spec_order() {
        assert_eq!(&DIAG_4X4[..6], &[(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(DIAG_4X4[15], (3, 3));
        assert_eq!(DIAG_2X2, [(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert_eq!(DIAG_8X8[63], (7, 7));
        // permutation checks
        for s in [&DIAG_8X8[..], &HORIZ_8X8[..], &VERT_8X8[..]] {
            let mut seen = [false; 64];
            for &(x, y) in s {
                assert!(!seen[(y as usize) * 8 + x as usize]);
                seen[(y as usize) * 8 + x as usize] = true;
            }
        }
    }

    #[test]
    fn dct_matrix_has_the_spec_sub_matrices() {
        // 4-point: rows 0, 8, 16, 24
        assert_eq!(&DCT32[0][..4], &[64, 64, 64, 64]);
        assert_eq!(&DCT32[8][..4], &[83, 36, -36, -83]);
        assert_eq!(&DCT32[16][..4], &[64, -64, -64, 64]);
        assert_eq!(&DCT32[24][..4], &[36, -83, 83, -36]);
        // 8-point row 1 = DCT32 row 4
        assert_eq!(&DCT32[4][..8], &[89, 75, 50, 18, -18, -50, -75, -89]);
        // 16-point row 1 = DCT32 row 2
        assert_eq!(&DCT32[2][..16], &[90, 87, 80, 70, 57, 43, 25, 9, -9, -25, -43, -57, -70, -80, -87, -90]);
        // 32-point row 1
        assert_eq!(&DCT32[1][..8], &[90, 90, 88, 85, 82, 78, 73, 67]);
        assert_eq!(DCT32[1][31], -90);
        assert_eq!(DCT32[31][0], 4);
        assert_eq!(DCT32[31][1], -13);
        // near-orthogonality: every row has energy ≈ 32·64²
        for k in 0..32 {
            let e: i64 = DCT32[k].iter().map(|&v| (v as i64) * (v as i64)).sum();
            assert!((e - 32 * 64 * 64).abs() < 32 * 64 * 64 / 50, "row {k} energy {e}");
        }
        for k in 1..32 {
            let dot: i64 = (0..32).map(|n| DCT32[0][n] as i64 * DCT32[k][n] as i64).sum();
            assert!(dot.abs() <= 64 * 4, "row {k} not orthogonal to DC: {dot}");
        }
    }

    #[test]
    fn misc_tables() {
        assert_eq!(INTRA_PRED_ANGLE[10], 0);
        assert_eq!(INTRA_PRED_ANGLE[26], 0);
        assert_eq!(INTRA_PRED_ANGLE[2], 32);
        assert_eq!(INTRA_PRED_ANGLE[18], -32);
        assert_eq!(INV_ANGLE[18 - 11], -256);
        assert_eq!(CHROMA_QP_420[29], 29);
        assert_eq!(CHROMA_QP_420[30], 29);
        assert_eq!(CHROMA_QP_420[43], 37);
        assert_eq!(CHROMA_QP_420[44], 38);
        assert_eq!(CHROMA_QP_420[57], 51);
        assert_eq!(BETA_TABLE[51], 64);
        assert_eq!(TC_TABLE[53], 24);
    }

    /// The padded DST mirror must agree with the table it mirrors.
    #[test]
    fn dst4_padding_matches_dst4() {
        for k in 0..4 {
            assert_eq!(&DST4_PAD[k][..4], &DST4[k][..], "row {k}");
            assert!(DST4_PAD[k][4..].iter().all(|&v| v == 0), "row {k} tail not zero");
        }
    }
}
