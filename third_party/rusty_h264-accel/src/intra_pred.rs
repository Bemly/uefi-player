//! **16x16 luma / 8x8 chroma intra prediction** — portable (rip-ASM Phase 5c).
//!
//! Replaces openh264's `WelsI16x16LumaPred{V,H,Dc,Plane}_sse2` and
//! `WelsIChromaPred{V,Plane}_sse2` (the last vendored intra_pred consumers).
//! The predictors are spec §8.3.3 forms — straight row copies (V), per-row
//! broadcasts (H), one average (DC) and a linear ramp (Plane); memory-shaped
//! work LLVM vectorises well, so these are deliberately plain Rust with
//! fixed-size inner loops. They were pinned bit-exact against the live
//! assembly by differential tests BEFORE the asm was ripped (2026-08-12);
//! the tests below now pin against an independent in-test transcription of
//! the same spec formulas.

/// 16x16 luma prediction into `pred` (>= 256 bytes). `rec[base]` is the MB
/// top-left; the top row is `rec[base - stride ..]`, the left column
/// `rec[base - 1 + y*stride]`. `mode`: 0=V, 1=H, 2=DC, 3=Plane — the caller
/// guarantees the needed neighbours exist (both for DC/Plane), exactly the
/// contract the asm wrapper had.
pub fn i16x16_luma_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 256);
    assert!(base >= stride + 1 && base + 15 * stride <= rec.len());
    match mode {
        0 => {
            let top = &rec[base - stride..base - stride + 16];
            for y in 0..16 {
                pred[y * 16..y * 16 + 16].copy_from_slice(top);
            }
        }
        1 => {
            for y in 0..16 {
                let l = rec[base - 1 + y * stride];
                pred[y * 16..y * 16 + 16].fill(l);
            }
        }
        2 => {
            let mut sum = 16u32;
            for i in 0..16 {
                sum += rec[base - stride + i] as u32;
                sum += rec[base - 1 + i * stride] as u32;
            }
            pred[..256].fill((sum >> 5) as u8);
        }
        _ => {
            // Spec §8.3.3.4. p[x,-1] = rec[base - stride + x], p[-1,y] = left,
            // p[-1,-1] = rec[base - stride - 1].
            let px = |x: isize| -> i32 {
                if x < 0 {
                    rec[base - stride - 1] as i32
                } else {
                    rec[base - stride + x as usize] as i32
                }
            };
            let py = |y: isize| -> i32 {
                if y < 0 {
                    rec[base - stride - 1] as i32
                } else {
                    rec[base - 1 + y as usize * stride] as i32
                }
            };
            let mut h = 0i32;
            let mut v = 0i32;
            for i in 0..8i32 {
                h += (i + 1) * (px(8 + i as isize) - px(6 - i as isize));
                v += (i + 1) * (py(8 + i as isize) - py(6 - i as isize));
            }
            let a = 16 * (px(15) + py(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16i32 {
                for x in 0..16i32 {
                    let p = (a + b * (x - 7) + c * (y - 7) + 16) >> 5;
                    pred[(y * 16 + x) as usize] = p.clamp(0, 255) as u8;
                }
            }
        }
    }
}

/// 8x8 chroma prediction into `pred` (>= 64 bytes). `mode`: 2=Vertical,
/// 3(=else)=Plane — the only modes the asm served; DC/Horizontal stay with the
/// caller's scalar, exactly the old wrapper's contract.
pub fn chroma8x8_pred(mode: u8, pred: &mut [u8], rec: &[u8], base: usize, stride: usize) {
    assert!(pred.len() >= 64);
    assert!(base >= stride + 1 && base + 7 * stride <= rec.len());
    if mode == 2 {
        let top = &rec[base - stride..base - stride + 8];
        for y in 0..8 {
            pred[y * 8..y * 8 + 8].copy_from_slice(top);
        }
    } else {
        // Spec §8.3.4.4 (chroma plane).
        let px = |x: isize| -> i32 {
            if x < 0 {
                rec[base - stride - 1] as i32
            } else {
                rec[base - stride + x as usize] as i32
            }
        };
        let py = |y: isize| -> i32 {
            if y < 0 {
                rec[base - stride - 1] as i32
            } else {
                rec[base - 1 + y as usize * stride] as i32
            }
        };
        let mut h = 0i32;
        let mut v = 0i32;
        for i in 0..4i32 {
            h += (i + 1) * (px(4 + i as isize) - px(2 - i as isize));
            v += (i + 1) * (py(4 + i as isize) - py(2 - i as isize));
        }
        let a = 16 * (px(7) + py(7));
        let b = (17 * h + 16) >> 5;
        let c = (17 * v + 16) >> 5;
        for y in 0..8i32 {
            for x in 0..8i32 {
                let p = (a + b * (x - 3) + c * (y - 3) + 16) >> 5;
                pred[(y * 8 + x) as usize] = p.clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    // VENDOR(uefi-player): test-only `Vec` import for the no_std default build.
    #[cfg(not(feature = "std"))]
    use alloc::vec::Vec;
    fn lcg(state: &mut u64) -> u32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*state >> 33) as u32
    }

    /// Aligned buffers (kept from the asm-oracle era: macroblock positions on
    /// aligned planes are 16-aligned, and keeping the tests on that geometry
    /// keeps them honest about production layouts).
    #[repr(align(16))]
    struct A<const N: usize>([u8; N]);

    /// Independent transcription of the spec predictors (per-pixel, no shared
    /// code with the implementation) — the oracle that replaced the ripped asm.
    fn ref_pred(n: usize, mode: u8, rec: &[u8], base: usize, stride: usize) -> Vec<u8> {
        let top = |x: isize| -> i32 {
            if x < 0 { rec[base - stride - 1] as i32 } else { rec[base - stride + x as usize] as i32 }
        };
        let left = |y: isize| -> i32 {
            if y < 0 { rec[base - stride - 1] as i32 } else { rec[base - 1 + y as usize * stride] as i32 }
        };
        let mut out = vec![0u8; n * n];
        match mode {
            0 => {
                for y in 0..n { for x in 0..n { out[y * n + x] = top(x as isize) as u8; } }
            }
            1 => {
                for y in 0..n { for x in 0..n { out[y * n + x] = left(y as isize) as u8; } }
            }
            2 => {
                let mut s = n as i32;
                for i in 0..n as isize { s += top(i) + left(i); }
                let dc = (s >> (if n == 16 { 5 } else { 4 })) as u8;
                out.fill(dc);
            }
            _ => {
                let half = n as i32 / 2;
                let (mut h, mut v) = (0i32, 0i32);
                for i in 0..half {
                    h += (i + 1) * (top((half + i) as isize) - top((half - 2 - i) as isize));
                    v += (i + 1) * (left((half + i) as isize) - left((half - 2 - i) as isize));
                }
                let a = 16 * (top(n as isize - 1) + left(n as isize - 1));
                let (b, c) = if n == 16 {
                    ((5 * h + 32) >> 6, (5 * v + 32) >> 6)
                } else {
                    ((17 * h + 16) >> 5, (17 * v + 16) >> 5)
                };
                for y in 0..n as i32 {
                    for x in 0..n as i32 {
                        let p = (a + b * (x - half + 1) + c * (y - half + 1) + 16) >> 5;
                        out[(y * n as i32 + x) as usize] = p.clamp(0, 255) as u8;
                    }
                }
            }
        }
        out
    }

    #[test]
    fn i16x16_pred_matches_asm_all_modes() {
        let mut st = 0xabcdu64;
        let stride = 64usize;
        for round in 0..500usize {
            let mut recb = A([0u8; 64 * 64]);
            for v in recb.0.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            let rec = &recb.0[..];
            // Deep inside the buffer, 16-aligned like a real MB position (the
            // asm's movdqa loads fault on anything else), varying by whole MBs.
            let base = 4 * stride + 16 + 16 * (round % 3);
            for mode in 0..4u8 {
                let mut a = A([0u8; 256]);
                super::i16x16_luma_pred(mode, &mut a.0, rec, base, stride);
                let want = ref_pred(16, mode, rec, base, stride);
                assert_eq!(&a.0[..], &want[..], "round {round} mode {mode}");
            }
        }
    }

    #[test]
    fn chroma8x8_pred_matches_asm_both_modes() {
        let mut st = 0x9999u64;
        let stride = 48usize;
        for round in 0..500usize {
            let mut recb = A([0u8; 48 * 48]);
            for v in recb.0.iter_mut() {
                *v = lcg(&mut st) as u8;
            }
            let rec = &recb.0[..];
            let base = 4 * stride + 16 + 8 * (round % 3);
            for mode in [2u8, 3] {
                let mut a = A([0u8; 64]);
                super::chroma8x8_pred(mode, &mut a.0, rec, base, stride);
                // chroma mode 2 = VERTICAL (ref mode 0), else Plane (ref mode 3).
                let want = ref_pred(8, if mode == 2 { 0 } else { 3 }, rec, base, stride);
                assert_eq!(&a.0[..], &want[..], "round {round} mode {mode}");
            }
        }
    }
}
