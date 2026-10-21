//! `gop_simd::kernels`：像素搬运三件套（fill 纯写 / copy 搬运 / convert 换色）。
//! 全 `asm!` 显式向量寄存器（防 LLVM 标量化）；AVX 系自带 `vzeroupper`。
//! 由主入口按门控轮转交替计时。拆分仅为满足单文件 ≤1000 行，不改行为。

use core::arch::asm;

// ---------- fill 变体：dst=显存WC，纯写 ----------

pub(crate) unsafe fn fill_scalar_u64(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let pat = (seed as u64) | ((seed as u64) << 32);
        let mut p = dst;
        let end = dst.add(len & !7);
        while p < end {
            (p as *mut u64).write_volatile(pat);
            p = p.add(8);
        }
    }
}

/// MMX 64 位（MMX 寄存器；128 位之前的 SIMD 对照组，收尾需 EMMS）。
/// 注：Rust asm 不允许 mmx_reg 作操作数，改用显式 mm0 + clobber。
pub(crate) unsafe fn fill_mmx64(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let pat = (seed as u64) | ((seed as u64) << 32);
        let mut p = dst;
        let end = dst.add(len & !7);
        while p < end {
            asm!("movq mm0, {v}", "movq [{d}], mm0",
                v = in(reg) pat, d = in(reg) p,
                out("mm0") _, options(nostack));
            p = p.add(8);
        }
        asm!("emms", options(nostack, preserves_flags));
    }
}

/// SSE1 128 位（movups，FP 域存）：与 SSE2 movdqu 同宽，做编码域对照。
pub(crate) unsafe fn fill_sse128(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movups [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "sse2")]
pub(crate) unsafe fn fill_sse2(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movdqu [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "sse2")]
pub(crate) unsafe fn fill_sse2_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("movntdq [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// AVX-128-VEX：同 128 位宽，VEX 编码对照组（看 VEX 本身有没有增益）。
#[target_feature(enable = "avx")]
pub(crate) unsafe fn fill_avx128(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        let v: u128 = (seed as u128)
            | ((seed as u128) << 32)
            | ((seed as u128) << 64)
            | ((seed as u128) << 96);
        let mut p = dst;
        let end = dst.add(len & !15);
        while p < end {
            asm!("vmovdqu [{d}], {v}", d = in(reg) p, v = in(xmm_reg) v, options(nostack));
            p = p.add(16);
        }
    }
}

#[target_feature(enable = "avx2")]
pub(crate) unsafe fn fill_avx2(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovdqu [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
    }
}

#[target_feature(enable = "avx2")]
pub(crate) unsafe fn fill_avx2_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovntdq [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

#[target_feature(enable = "avx512f")]
pub(crate) unsafe fn fill_avx512(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm512_set1_epi32;
        let v = _mm512_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !63);
        while p < end {
            asm!("vmovdqu8 [{d}], {v}", d = in(reg) p, v = in(zmm_reg) v, options(nostack));
            p = p.add(64);
        }
    }
}

#[target_feature(enable = "avx512f")]
pub(crate) unsafe fn fill_avx512_nt(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm512_set1_epi32;
        let v = _mm512_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !63);
        while p < end {
            asm!("vmovntdq [{d}], {v}", d = in(reg) p, v = in(zmm_reg) v, options(nostack));
            p = p.add(64);
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// AVX512VL-256：同 256 位宽，EVEX 编码对照组。
#[target_feature(enable = "avx512f,avx512vl")]
pub(crate) unsafe fn fill_avx512vl256(dst: *mut u8, len: usize, seed: u32) {
    unsafe {
        use core::arch::x86_64::_mm256_set1_epi32;
        let v = _mm256_set1_epi32(seed as i32);
        let mut p = dst;
        let end = dst.add(len & !31);
        while p < end {
            asm!("vmovdqu8 [{d}], {v}", d = in(reg) p, v = in(ymm_reg) v, options(nostack));
            p = p.add(32);
        }
    }
}

#[inline(always)]
pub(crate) unsafe fn vzeroupper() {
    unsafe {
        asm!("vzeroupper", options(nostack, preserves_flags));
    }
}

// ---------- copy 变体：src=WB内存 → dst=显存WC ----------

pub(crate) unsafe fn copy_scalar_u64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            let w = (src.add(i) as *const u64).read_unaligned();
            (dst.add(i) as *mut u64).write_volatile(w);
            i += 8;
        }
    }
}

/// MMX 64 位拷贝（对照组，收尾 EMMS）。
pub(crate) unsafe fn copy_mmx64(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        while i + 8 <= len {
            asm!("movq mm0, [{s}]", "movq [{d}], mm0",
                s = in(reg) src.add(i), d = in(reg) dst.add(i),
                out("mm0") _, options(nostack));
            i += 8;
        }
        asm!("emms", options(nostack, preserves_flags));
    }
}

/// SSE1 128 位拷贝（movups 载+存）。
pub(crate) unsafe fn copy_sse128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movups {t}, [{s}]", "movups [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

// 注：SSE4.1 的唯一纯内存原语 movntdqa 要求源为 WC 类型，对普通 RAM 会 #GP
//（QEMU 实测 #GP 实锤），故不用于 copy；SSE4.1 改用 pmovzxbw 体现在拆包行。

/// AVX VEX-128 拷贝：与 SSE2 同宽，换 VEX 编码（3 操作数、无破坏）。
#[target_feature(enable = "avx")]
pub(crate) unsafe fn copy_avx128(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

#[target_feature(enable = "sse2")]
pub(crate) unsafe fn copy_sse2(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

/// SSE3-LDDQU：非对齐视频源专用 load，对齐未知源的对照组。
#[target_feature(enable = "sse3")]
pub(crate) unsafe fn copy_sse3_lddqu(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("lddqu {t}, [{s}]", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
    }
}

#[target_feature(enable = "sse2")]
pub(crate) unsafe fn copy_sse2_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !15;
        while i < n {
            asm!("movdqu {t}, [{s}]", "movntdq [{d}], {t}",
                t = out(xmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 16;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

#[target_feature(enable = "avx2")]
pub(crate) unsafe fn copy_avx2(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
    }
}

#[target_feature(enable = "avx2")]
pub(crate) unsafe fn copy_avx2_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            asm!("vmovdqu {t}, [{s}]", "vmovntdq [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

/// prefetch 扫描：hint 0=无，1=nta，2=prefetchw，3=t0，4=t1，5=t2。
/// load 侧每 512B 预取一次。
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn copy_avx2_pf(dst: *mut u8, src: *const u8, len: usize, hint: u8) {
    unsafe {
        let mut i = 0;
        let n = len & !31;
        while i < n {
            match hint {
                1 => {
                    asm!("prefetchnta [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                2 => {
                    asm!("prefetchw [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                3 => {
                    asm!("prefetcht0 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                4 => {
                    asm!("prefetcht1 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                5 => {
                    asm!("prefetcht2 [{p}]", p = in(reg) src.add(i + 512), options(nostack, preserves_flags))
                }
                _ => {}
            }
            asm!("vmovdqu {t}, [{s}]", "vmovdqu [{d}], {t}",
                t = out(ymm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 32;
        }
    }
}

#[target_feature(enable = "avx512f")]
pub(crate) unsafe fn copy_avx512(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovdqu8 [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
    }
}

#[target_feature(enable = "avx512f")]
pub(crate) unsafe fn copy_avx512_nt(dst: *mut u8, src: *const u8, len: usize) {
    unsafe {
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {t}, [{s}]", "vmovntdq [{d}], {t}",
                t = out(zmm_reg) _, s = in(reg) src.add(i), d = in(reg) dst.add(i), options(nostack));
            i += 64;
        }
        asm!("sfence", options(nostack, preserves_flags));
    }
}

// ---------- 换色：BGRX→RGBX（播放管线每帧在 CPU 干的活） ----------

pub(crate) unsafe fn convert_scalar(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        for i in 0..px {
            let s = src.add(i * 4);
            let d = dst.add(i * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX VEX-128 vpshufb：与 SSSE3 pshufb 同宽同语义，换 VEX 编码对照。
#[target_feature(enable = "avx")]
pub(crate) unsafe fn convert_vpshufb128(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{__m128i, _mm_set_epi8};
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            let mut t: __m128i;
            asm!("vmovdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(xmm_reg) t, m = in(xmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// SSSE3 pshufb：16B=4 像素一次，mask=[2,1,0,3]×4。
#[target_feature(enable = "ssse3")]
pub(crate) unsafe fn convert_pshufb(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::_mm_set_epi8;
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            asm!("movdqu {t}, [{s}]", "pshufb {t}, {m}", "movdqu [{d}], {t}",
                t = out(xmm_reg) _, m = in(xmm_reg) m,
                s = in(reg) src.add(i * 4), d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX2 vpshufb：32B=8 像素一次（lane 内 shuffle，4B 像素不跨 lane，mask 复用）。
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn convert_vpshufb256(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{_mm_set_epi8, _mm256_set_m128i};
        let m128 = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let m = _mm256_set_m128i(m128, m128);
        let mut i = 0;
        let n = px & !7;
        while i < n {
            let mut t: core::arch::x86_64::__m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(ymm_reg) t, m = in(ymm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 8;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX512BW vpshufb：64B=16 像素一次，mask 由 128 位广播×4（lane 内语义相同）。
#[target_feature(enable = "avx512f,avx512bw")]
pub(crate) unsafe fn convert_vpshufb512(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{_mm_set_epi8, _mm512_broadcast_i32x4};
        let m128 = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let m = _mm512_broadcast_i32x4(m128);
        let mut i = 0;
        let n = px & !15;
        while i < n {
            let mut t: core::arch::x86_64::__m512i;
            asm!("vmovdqu8 {t}, [{s}]", t = out(zmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpshufb {t}, {t}, {m}", "vmovdqu8 [{d}], {t}",
                t = inout(zmm_reg) t, m = in(zmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 16;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AVX512VBMI vpermb：512 位任意字节置换（pshufb 只能 lane 内，VBMI 可跨 lane）。
pub(crate) const PERMB_RGBA: [u8; 64] = {
    let mut a = [0u8; 64];
    let mut i = 0;
    while i < 16 {
        a[i * 4] = (i * 4 + 2) as u8;
        a[i * 4 + 1] = (i * 4 + 1) as u8;
        a[i * 4 + 2] = (i * 4) as u8;
        a[i * 4 + 3] = (i * 4 + 3) as u8;
        i += 1;
    }
    a
};

#[target_feature(enable = "avx512f,avx512bw,avx512vbmi")]
pub(crate) unsafe fn convert_vpermb(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::__m512i;
        let mut m: __m512i;
        asm!("vmovdqu8 {m}, [{p}]", m = out(zmm_reg) m, p = in(reg) PERMB_RGBA.as_ptr(), options(nostack));
        let mut i = 0;
        let n = px & !15;
        while i < n {
            let mut t: __m512i;
            asm!("vmovdqu8 {t}, [{s}]", t = out(zmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpermb {t}, {t}, {m}", "vmovdqu8 [{d}], {t}",
                t = inout(zmm_reg) t, m = in(zmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 16;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}

/// AMD XOP vpperm：128 位按字节置换（控制位低 4 位选源字节）。
/// 注：rustc 的 xop target_feature 尚未稳定，直接用 asm 发射，运行时门控。
pub(crate) unsafe fn convert_vpperm_xop(dst: *mut u8, src: *const u8, px: usize) {
    unsafe {
        use core::arch::x86_64::{__m128i, _mm_set_epi8};
        let m = _mm_set_epi8(15, 12, 13, 14, 11, 8, 9, 10, 7, 4, 5, 6, 3, 0, 1, 2);
        let mut i = 0;
        let n = px & !3;
        while i < n {
            let mut t: __m128i;
            asm!("vmovdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) src.add(i * 4), options(nostack));
            asm!("vpperm {t}, {t}, {t}, {m}", "vmovdqu [{d}], {t}",
                t = inout(xmm_reg) t, m = in(xmm_reg) m,
                d = in(reg) dst.add(i * 4), options(nostack));
            i += 4;
        }
        for j in i..px {
            let s = src.add(j * 4);
            let d = dst.add(j * 4);
            let b = s.read();
            let r = s.add(2).read();
            d.write(r);
            d.add(1).write(s.add(1).read());
            d.add(2).write(b);
            d.add(3).write(s.add(3).read());
        }
    }
}
