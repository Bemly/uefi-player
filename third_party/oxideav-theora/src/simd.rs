//! VENDOR(uefi-player): `simd` feature — SSE2-vectorized twins of the §7.9/§7.10 hot paths.
//!
//! 搜过什么、为什么不用/用（2026-10-21 实搜，见 VENDOR.md §"SIMD 卷土重来"）：
//! - `wide 1.7.1`（84.0M 下载，MSRV 1.89，Zlib/Apache-2.0/MIT，
//!   `default-features = false` 可 no_std）——**用**。`i16x8::mul_keep_high`
//!   就是 butterfly 要的 `(a*b)>>16`（`pmulhw`，SSE2）；另有 wrapping 加减、
//!   `saturating_add`（`paddsw`，重建钳制）、`min`/`max`（`pminsw/pmaxsw`）、
//!   `simd_gt`+`select`（`pcmpgtw`+与/或，`lflim` 去分支）、
//!   `from_u8x16_low/high`（`punpcklbw`，零扩展）、`narrow_i16x8`
//!   （`packuswb`，饱和钳制）、`from_i32x8_truncate`。与在树 h263-rs 同版本，
//!   无新增 lockfile 条目。其 `std` 特性只加速 f32/f64 `sqrt`，整数核零触及。
//! - `safe_arch 1.2.0`（82.7M，同 license，no_std 原生）——不用：`core::arch`
//!   薄包装 = 内在函数，无 XSAVE 集成，相对 wide 零附加价值（AGENTS 要
//!   `asm!` 优先，h263 前例已在同机上选 wide 弃 safe_arch）。
//! - `pulp 0.22.3`（31.9M，MIT）——不用：默认 `x86-v3`（AVX2）基线撞 UEFI
//!   SSE2 基线 + XSAVE 门控，且自带分发（禁第二套 CPUID）。
//! - wide 的 runtime-index `shuffle` 在纯 SSE2 下标量化（`u16x8_.rs` 的
//!   `pick!` 在无 SSSE3 时走 `to_array` 标量回退）：55 步 butterfly 的配对每级
//!   都变，跨车道重排不可避免，故**不按 8 车道做 1 个变换**，而是把 8 行变换
//!   一次跑完（见下）；仅有的两次 8×8 转置用 `core::arch` 的
//!   `punpcklwd/hwd` 手排（SSE2 基线 1:1 指令，无标量化风险），函数级
//!   `#[allow(unsafe_code)]` + SAFETY（其余全文件 safe，crate 级
//!   `#![deny(unsafe_code)]` 保持）。
//!
//! 并行形状（SIMD-friendly 算法重排，逐位一致证明见 VENDOR.md）：
//! 55 步 butterfly 每步都是单 `t[i]` 标量 op——**一个变换内无 SIMD 并行度**。
//! 并行度来自 8 个独立变换：8 个 `i16x8` 寄存器 `V0..V7`，`Vk` 装 8 个行变换
//! 的第 `k` 个元（列优先 SoA）。55 步逐字映射为车道 op（`t[i]` → `Vi`）：
//! 加减 wrapping（`paddw/psubw`，与标量 `wrapping_*` 同余）、乘法高半部
//! （`pmulhw`，输入恒为标量先截断到 i16 的值，见 `mul_bigc` 注释）。
//! 下游全是线性 op + 显式 `as i16` 截断，故车道 mod-2^16 与标量 i32 全程同余，
//! 截断点/输出逐位一致。行列两遍共用同一 `idct_8wide`，两遍之间一次转置
//! （列遍消费转置后的中间行——kernel 跨输入向量 gather 变换，布局恰好衔接）。
//! 反量化保持标量（zig-zag gather 在 SSE2 上 hostile：`pinsrw` 串行链与
//! store-forwarding 碎片都比标量 imul 环慢 2.4 倍，已实测回退，见 VENDOR.md），
//! 其输出行经 `movdqu` 直接进入转置-in（0.8ns，可忽略）。
//! 重建 2(f) 融合成 `paddsw`+`packuswb` 单遍；环路滤波 `lflim` 去分支
//! （SSE2 mask-select）+ 双边 8 行/列向量化 + `packuswb` 钳制；车道填充一律
//! 经 `load_i16x8`（`black_box` 挡住 LLVM 的 `pinsrw` SLP，见该函数注释）。
//!
//! 运行时开关（`[accel]` 单比特座子）：`set_simd_enabled` 公开，固件按
//! `copy_width()` 推（`scalar`=关/其余=开，`resolve_accel` 后，见 VENDOR.md
//! 固件契约）；本箱永不自研 CPUID 探测，只服从开关。`Relaxed` 足够——双路
//! 逐字节一致（互锁单测），撕裂读至多在一帧内混用两路，不损坏输出。
//! `std` 下默认 ON（与上游行为一致），no_std 下 fail-closed OFF。

#[cfg(feature = "simd")]
use wide::{i16x8, u8x16};

#[cfg(feature = "simd")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "simd")]
static SIMD_ENABLED: AtomicBool = AtomicBool::new(cfg!(feature = "std"));

/// Enable (`true`) or disable (`false`) the SSE2 fast paths at runtime.
/// No-op unless the `simd` feature is on, so callers need no `cfg`.
pub fn set_simd_enabled(en: bool) {
    #[cfg(feature = "simd")]
    SIMD_ENABLED.store(en, Ordering::Relaxed);
    #[cfg(not(feature = "simd"))]
    let _ = en;
}

#[cfg(feature = "simd")]
pub(crate) fn simd_enabled() -> bool {
    SIMD_ENABLED.load(Ordering::Relaxed)
}

/// Load 8 staged i16 lanes with one `movdqu`.
///
/// Without the `black_box` LLVM SLP-forwards scalar lane values into
/// `pinsrw` serial chains on the gather paths (measured slower than the
/// scalar loop); the barrier forces stack materialization so this compiles
/// to 8×`movw` stores + 1×`movdqu` (all L1, parallel). Zero runtime cost
/// (pure compiler fence, `core`, stable since 1.66).
#[cfg(feature = "simd")]
#[inline]
fn load_i16x8(staged: &[i16; 8]) -> i16x8 {
    i16x8::from_slice_unaligned(core::hint::black_box(staged))
}

// ---------------------------------------------------------------------------
// §7.9.3.1 1D IDCT — 8 transforms at once (§7.9.3.2 row/column passes share it)
// ---------------------------------------------------------------------------

/// Signed-16-bit views of the §7.9.3.1 Table 7.65 constants that exceed
/// `i16::MAX`: `pmulhw` treats both lanes as *signed*, so a constant like
/// `S7 = 64277` must enter as `64277 − 65536 = −1259`; the missing `+y`
/// is added back by [`mul_bigc`] (exactness proof there).
#[cfg(feature = "simd")]
mod bigc {
    use super::super::{
        IDCT_C3, IDCT_C4, IDCT_C6, IDCT_C7, IDCT_S3, IDCT_S6, IDCT_S7,
    };
    pub(super) const C3S: i16 = (IDCT_C3 - 65536) as i16;
    pub(super) const C4S: i16 = (IDCT_C4 - 65536) as i16;
    pub(super) const C6V: i16 = IDCT_C6 as i16;
    pub(super) const C7V: i16 = IDCT_C7 as i16;
    pub(super) const S3S: i16 = (IDCT_S3 - 65536) as i16;
    pub(super) const S6S: i16 = (IDCT_S6 - 65536) as i16;
    pub(super) const S7S: i16 = (IDCT_S7 - 65536) as i16;
}

/// `(y*C)>>16` for a Table 7.65 constant `C` in `(32767, 65536)`.
///
/// `pmulhw` needs both lanes signed, so the constant enters as the negative
/// `c_signed = C − 65536` and the identity
/// `(y*C)>>16 = ((y*c_signed)>>16) + y` closes the gap: `y*65536` is a
/// multiple of `2^16`, and the arithmetic shift distributes *exactly* over
/// the addition of a multiple (`(P + k*2^16)>>16 = (P>>16) + k`).
/// The trailing `+ y` is wrapping `paddw`: lanes track scalar values mod 2^16
/// (downstream is linear until the explicit `as i16` truncations — proof in
/// VENDOR.md), so the wrap is congruent, never divergent.
/// Constants `C6`/`C7` fit `i16` and use plain `mul_keep_high`.
#[cfg(feature = "simd")]
#[inline]
fn mul_bigc(y: i16x8, c_signed: i16x8) -> i16x8 {
    y.mul_keep_high(c_signed) + y
}

/// The §7.9.3.1 55-step butterfly on 8 independent transforms at once.
///
/// `y[k]` holds element `k` of 8 parallel 1D transforms (column `k` of eight
/// rows); the return holds the 8 outputs in the same layout. Steps mirror
/// `inverse_dct_1d_scalar` line-for-line (`t[i]` → lane register `ti`,
/// `Y[i]` → `y[i]`); wrapping add/sub and `mul_keep_high` (+ [`mul_bigc`])
/// are the lane images of the scalar `wrapping_*` / `>> 16` steps.
///
/// Register discipline: constants are splatted *at use* (short live range)
/// rather than hoisted — 8 data lanes + 1 const + temps fit the 16 xmm
/// registers with no spills (hoisting all 7 keeps 15 live and spills).
#[cfg(feature = "simd")]
pub(crate) fn idct_8wide(y: &[i16x8; 8]) -> [i16x8; 8] {
    use bigc::{C3S, C4S, C6V, C7V, S3S, S6S, S7S};
    // Steps 1–6.
    let mut t0 = y[0] + y[4];
    t0 = mul_bigc(t0, i16x8::splat(C4S));
    let mut t1 = y[0] - y[4];
    t1 = mul_bigc(t1, i16x8::splat(C4S));
    // Steps 7–12.
    let t2 = y[2].mul_keep_high(i16x8::splat(C6V)) - mul_bigc(y[6], i16x8::splat(S6S));
    let t3 = mul_bigc(y[2], i16x8::splat(S6S)) + y[6].mul_keep_high(i16x8::splat(C6V));
    let t4 = y[1].mul_keep_high(i16x8::splat(C7V)) - mul_bigc(y[7], i16x8::splat(S7S));
    let t5 = mul_bigc(y[5], i16x8::splat(C3S)) - mul_bigc(y[3], i16x8::splat(S3S));
    let t6 = mul_bigc(y[5], i16x8::splat(S3S)) + mul_bigc(y[3], i16x8::splat(C3S));
    let t7 = mul_bigc(y[1], i16x8::splat(S7S)) + y[7].mul_keep_high(i16x8::splat(C7V));
    let (mut t2, mut t3, mut t4, mut t5, mut t6, mut t7) = (t2, t3, t4, t5, t6, t7);

    // Steps 13–17: R = T[4] + T[5]; T[5] = C4*(T[4] − T[5]) >> 16; T[4] = R.
    let r = t4 + t5;
    let d = t4 - t5;
    t5 = mul_bigc(d, i16x8::splat(C4S));
    t4 = r;
    // Steps 18–22: R = T[7] + T[6]; T[6] = C4*(T[7] − T[6]) >> 16; T[7] = R.
    let r = t7 + t6;
    let d = t7 - t6;
    t6 = mul_bigc(d, i16x8::splat(C4S));
    t7 = r;
    // Steps 23–25: R = T[0] + T[3]; T[3] = T[0] − T[3]; T[0] = R.
    let r = t0 + t3;
    t3 = t0 - t3;
    t0 = r;
    // Steps 26–28: R = T[1] + T[2]; T[2] = T[1] − T[2]; T[1] = R.
    let r = t1 + t2;
    t2 = t1 - t2;
    t1 = r;
    // Steps 29–31: R = T[6] + T[5]; T[5] = T[6] − T[5]; T[6] = R.
    let r = t6 + t5;
    t5 = t6 - t5;
    t6 = r;

    // Steps 32–55: 8 outputs, each a wrapping add/sub truncated to i16
    // (lane add/sub already wraps exactly like the scalar `as i16`).
    [
        t0 + t7,
        t1 + t6,
        t2 + t5,
        t3 + t4,
        t3 - t4,
        t2 - t5,
        t1 - t6,
        t0 - t7,
    ]
}

// ---------------------------------------------------------------------------
// §7.9.2 → §7.9.3.2 fused: dequantize gathers columns, IDCT runs 8-wide
// ---------------------------------------------------------------------------

/// The §7.9.3.2 column-pass tail `(X[ri] + 8) >> 4` on one 8-wide column.
///
/// Exact i16-lane decomposition — no i32 widening (which costs two
/// unpacks, two shifts and a pack per column on SSE2):
/// `(X+8)>>4 = (X>>4) + (((X&15)+8)>>4)`, using the mathematical mod
/// (`X&15` is `X mod 16` in two's complement, including for negative `X`;
/// both summands fit i16 with margin). All four ops are single SSE2
/// instructions (`psraw/pand/paddw`).
#[cfg(feature = "simd")]
fn idct_bias_col(v: i16x8) -> i16x8 {
    let base = v.unbounded_shr_scalar(4);
    let frac = ((v & i16x8::splat(15)) + i16x8::splat(8)).unbounded_shr_scalar(4);
    base + frac
}

/// 8×8 transpose of `i16` lanes (row-major in, row-major out).
///
/// Standard three-stage SSE2 network (`punpcklwd/hwd` → `punpckldq/hdq` →
/// `punpcklqdq/hqdq`), 24 instructions. Pure data movement, hence exact.
/// `core::arch` (not wide: wide exposes no 16-bit unpack, and its
/// runtime-index `shuffle` scalarizes without SSSE3) — x86_64 only, with a
/// scalar fallback elsewhere for portability.
#[cfg(feature = "simd")]
pub(crate) fn transpose_8x8(m: [i16x8; 8]) -> [i16x8; 8] {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: all operands are live `__m128i` values converted from/to
        // `wide` vectors (same register representation); `loadu`/`storeu`
        // need no alignment; `punpck*` only shuffles lanes; SSE2 is baseline
        // on x86_64 (no target-feature gate needed, no XSAVE interaction —
        // the `[accel]` switch is config semantics, not a safety gate).
        #[allow(unsafe_code)]
        fn unpack(
            f: unsafe fn(
                core::arch::x86_64::__m128i,
                core::arch::x86_64::__m128i,
            ) -> core::arch::x86_64::__m128i,
            a: i16x8,
            b: i16x8,
        ) -> i16x8 {
            unsafe { i16x8::from(f(a.into(), b.into())) }
        }
        #[allow(unsafe_code)]
        fn run(m: [i16x8; 8]) -> [i16x8; 8] {
            use core::arch::x86_64::{
                _mm_unpackhi_epi16, _mm_unpackhi_epi32, _mm_unpackhi_epi64,
                _mm_unpacklo_epi16, _mm_unpacklo_epi32, _mm_unpacklo_epi64,
            };
            // Stage 1: interleave 16-bit lanes of row pairs.
            let t0 = unpack(_mm_unpacklo_epi16, m[0], m[1]);
            let t1 = unpack(_mm_unpackhi_epi16, m[0], m[1]);
            let t2 = unpack(_mm_unpacklo_epi16, m[2], m[3]);
            let t3 = unpack(_mm_unpackhi_epi16, m[2], m[3]);
            let t4 = unpack(_mm_unpacklo_epi16, m[4], m[5]);
            let t5 = unpack(_mm_unpackhi_epi16, m[4], m[5]);
            let t6 = unpack(_mm_unpacklo_epi16, m[6], m[7]);
            let t7 = unpack(_mm_unpackhi_epi16, m[6], m[7]);
            // Stage 2: interleave 32-bit lanes.
            let u0 = unpack(_mm_unpacklo_epi32, t0, t2);
            let u1 = unpack(_mm_unpackhi_epi32, t0, t2);
            let u2 = unpack(_mm_unpacklo_epi32, t1, t3);
            let u3 = unpack(_mm_unpackhi_epi32, t1, t3);
            let u4 = unpack(_mm_unpacklo_epi32, t4, t6);
            let u5 = unpack(_mm_unpackhi_epi32, t4, t6);
            let u6 = unpack(_mm_unpacklo_epi32, t5, t7);
            let u7 = unpack(_mm_unpackhi_epi32, t5, t7);
            // Stage 3: interleave 64-bit lanes → columns become rows.
            [
                unpack(_mm_unpacklo_epi64, u0, u4),
                unpack(_mm_unpackhi_epi64, u0, u4),
                unpack(_mm_unpacklo_epi64, u1, u5),
                unpack(_mm_unpackhi_epi64, u1, u5),
                unpack(_mm_unpacklo_epi64, u2, u6),
                unpack(_mm_unpackhi_epi64, u2, u6),
                unpack(_mm_unpacklo_epi64, u3, u7),
                unpack(_mm_unpackhi_epi64, u3, u7),
            ]
        }
        run(m)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut out = [i16x8::ZERO; 8];
        for (r, row) in m.iter().enumerate() {
            let a = row.to_array();
            for (c, slot) in out.iter_mut().enumerate() {
                let mut col = slot.to_array();
                col[r] = a[c];
                *slot = i16x8::new(col);
            }
        }
        out
    }
}

/// SIMD twin of [`super::inverse_dct_2d`]: transpose-in (8 row loads + one
/// 8×8 unpack transpose, 0.8ns — pure L1) → shared 8-wide row pass →
/// transpose → shared 8-wide column pass → `(X+8)>>4` tails → natural rows
/// (no transpose-out: each column-pass transform's outputs land in its own
/// lane, so the output rows are already natural).
#[cfg(feature = "simd")]
pub(crate) fn inverse_dct_2d_simd(dqc: &[i16; 64]) -> [[i16; 8]; 8] {
    let mut rows = [i16x8::ZERO; 8];
    for (r, dst) in rows.iter_mut().enumerate() {
        *dst = i16x8::from_slice_unaligned(&dqc[r * 8..r * 8 + 8]);
    }
    let cols = transpose_8x8(rows);
    // Row pass over the columns (transform m = row m)…
    let interm_t = idct_8wide(&cols);
    // …column pass over the transposed intermediate (transform m =
    // column m of the row-pass output)…
    let rows = transpose_8x8(interm_t);
    let out_t = idct_8wide(&rows);
    // …then the (§7.9.3.2) tail per element; output rows are natural.
    let mut res = [[0i16; 8]; 8];
    for (dst, row) in res.iter_mut().zip(out_t.iter()) {
        dst.copy_from_slice(&idct_bias_col(*row).to_array());
    }
    res
}

/// SIMD twin of the §7.9.4 step 2(f) add-and-clamp + runtime dispatch.
///
/// `PRED` is `u8` (0..=255), `RES` is `i16`; the scalar widens the sum to
/// i32 and clamps to `0..=255`. Lane version: zero-extended `paddsw`
/// (`saturating_add`) then `packuswb` (`narrow_i16x8`): the i16 sum can only
/// reach `[-32768, 33022]` (`255 + 32767`), so `paddsw` saturates exactly
/// where the scalar later clamps to 255, and `packuswb` saturates the rest —
/// the composition is the scalar clamp, bit for bit (proof in VENDOR.md).
#[cfg(feature = "simd")]
fn add_pred_res_clamp_simd(
    pred: &[[u8; 8]; 8],
    res: &[[i16; 8]; 8],
) -> [[u8; 8]; 8] {
    let mut out = [[0u8; 8]; 8];
    for r in 0..4 {
        let mut p0 = [0i16; 8];
        let mut p1 = [0i16; 8];
        for c in 0..8 {
            p0[c] = pred[2 * r][c] as i16;
            p1[c] = pred[2 * r + 1][c] as i16;
        }
        let s0 =
        load_i16x8(&p0).saturating_add(i16x8::from_slice_unaligned(&res[2 * r]));
        let s1 = load_i16x8(&p1)
        .saturating_add(i16x8::from_slice_unaligned(&res[2 * r + 1]));
        let bytes = u8x16::narrow_i16x8(s0, s1).to_array();
        out[2 * r].copy_from_slice(&bytes[..8]);
        out[2 * r + 1].copy_from_slice(&bytes[8..]);
    }
    out
}

pub(crate) fn add_pred_res_clamp(
    pred: &[[u8; 8]; 8],
    res: &[[i16; 8]; 8],
) -> [[u8; 8]; 8] {
    #[cfg(feature = "simd")]
    if simd_enabled() {
        return add_pred_res_clamp_simd(pred, res);
    }
    super::add_pred_res_clamp_scalar(pred, res)
}

// ---------------------------------------------------------------------------
// §7.10 loop filter — branchless `lflim` + 8-wide edge cores + pack clamp
// ---------------------------------------------------------------------------

/// Branchless `lflim(R, L)` on 8 lanes (SSE2 mask-select shape, no `blendv`:
/// `pcmpgtw` + `pand/pandn/por` via `select`).
///
/// `R` here is the 4-tap response (fits i16: `|R| <= 1024` for u8 pixels);
/// `l` is the frame limit (`u8`, splatted by the caller). Derivation: for
/// `r >= 0` the tent is `max(min(r, 2l-r), 0)`; oddness extends it to
/// `r < 0`; clamping to `[-2l, 2l]` first makes the out-of-band case select
/// 0. Every op is `pminsw/pmaxsw/paddw/psubw/pcmpgtw/select` — all SSE2.
/// Exhaustively interlocked against scalar `lflim` at every breakpoint ±2
/// for every `l` (see tests): bit-exact, including `l = 0`.
#[cfg(feature = "simd")]
pub(crate) fn lflim_8wide(r: i16x8, l: i16) -> i16x8 {
    let zero = i16x8::ZERO;
    let lim = i16x8::splat(l);
    let two = lim + lim;
    let neg_two = zero - two;
    let neg_lim = zero - lim;
    // Clamp into the tent support; anything beyond selects 0 below.
    let clamped = r.max(neg_two).min(two);
    // Rising edge (clamped > l): fold to 2l − clamped.
    let folded = clamped.simd_gt(lim).select(two - clamped, clamped);
    // Falling edge (folded < −l): fold to −2l − folded.
    let folded = folded.simd_lt(neg_lim).select(neg_two - folded, folded);
    // Out of band (|r| > 2l): genuine edge, untouched (0).
    let over = r.simd_gt(two) | r.simd_lt(neg_two);
    over.select(zero, folded)
}

/// SIMD core of `horizontal_loop_filter_edge` (guards stay in `lib.rs`):
/// 8 rows × 4 tap bytes in, updated `(p1', p2')` per row out.
///
/// `R = (p0 − 3·p1 + 3·p2 − p3 + 4) >> 3` is lane-local (`pmullw` by 3 is
/// exact: `3·255 = 765`, no wrap; `|R| <= 1024`). `delta = lflim(R, L)` is
/// the branchless 8-wide tent; `p1 + delta` / `p2 − delta` stay in
/// `[-255, 510]` (i16-exact, no wrap) and `packuswb` saturates exactly like
/// the spec's three-way clamp. Plane loads/stores stay scalar (rows are
/// stride-apart; only the compute is 8-wide).
#[cfg(feature = "simd")]
pub(crate) fn h_filter_rows(rows: &[[u8; 4]; 8], l: u8) -> [[u8; 2]; 8] {
    let mut p0 = [0i16; 8];
    let mut p1 = [0i16; 8];
    let mut p2 = [0i16; 8];
    let mut p3 = [0i16; 8];
    for (r, row) in rows.iter().enumerate() {
        p0[r] = row[0] as i16;
        p1[r] = row[1] as i16;
        p2[r] = row[2] as i16;
        p3[r] = row[3] as i16;
    }
    let (p0, p1, p2, p3) = (
        load_i16x8(&p0),
        load_i16x8(&p1),
        load_i16x8(&p2),
        load_i16x8(&p3),
    );
    let three = i16x8::splat(3);
    let r = p0 - p1 * three + p2 * three - p3 + i16x8::splat(4);
    let r = r.unbounded_shr_scalar(3);
    let d = lflim_8wide(r, l as i16);
    let bytes = u8x16::narrow_i16x8(p1 + d, p2 - d).to_array();
    let mut out = [[0u8; 2]; 8];
    for (r, slot) in out.iter_mut().enumerate() {
        slot[0] = bytes[r];
        slot[1] = bytes[8 + r];
    }
    out
}

/// SIMD core of `vertical_loop_filter_edge` (guards stay in `lib.rs`):
/// 4 rows × 8 bytes in (one column per lane), updated rows `fy+1`/`fy+2` out.
///
/// Same arithmetic as [`h_filter_rows`], but columns are lanes natively, so
/// the packed result is two *contiguous* rows — no scatter.
#[cfg(feature = "simd")]
pub(crate) fn v_filter_cols(cols: &[[u8; 8]; 4], l: u8) -> [[u8; 8]; 2] {
    let mut p = [[0i16; 8]; 4];
    for (j, row) in cols.iter().enumerate() {
        for (c, b) in row.iter().enumerate() {
            p[j][c] = *b as i16;
        }
    }
    let (p0, p1, p2, p3) = (
        load_i16x8(&p[0]),
        load_i16x8(&p[1]),
        load_i16x8(&p[2]),
        load_i16x8(&p[3]),
    );
    let three = i16x8::splat(3);
    let r = p0 - p1 * three + p2 * three - p3 + i16x8::splat(4);
    let r = r.unbounded_shr_scalar(3);
    let d = lflim_8wide(r, l as i16);
    let bytes = u8x16::narrow_i16x8(p1 + d, p2 - d).to_array();
    let mut out = [[0u8; 8]; 2];
    out[0].copy_from_slice(&bytes[..8]);
    out[1].copy_from_slice(&bytes[8..]);
    out
}
