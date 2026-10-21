//! `gop_simd::misc`：校验/密码/RNG/GPR 像素 helper（数据通路吞吐对照）。
//! `checksum`/`density` 帧级内容，`crypto_*` 数据通路，`rng_*` 生成，`gpr_*` 标量寄存器。
//! 由主入口按门控计时。拆分仅为满足单文件 ≤1000 行，不改行为。

use core::arch::asm;

// ---------- 校验/内容：帧级 checksum 与位密度 ----------

pub(crate) unsafe fn checksum_scalar(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            acc = acc.wrapping_add((buf.add(i) as *const u64).read_unaligned());
            i += 8;
        }
        acc
    }
}

/// SSE4.2 crc32：8B 一步，帧完整性校验的硬件路径。
#[target_feature(enable = "sse4.2")]
pub(crate) unsafe fn checksum_crc32(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut crc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            let w = (buf.add(i) as *const u64).read_unaligned();
            asm!("crc32 {c}, {w}", c = inout(reg) crc, w = in(reg) w, options(nostack));
            i += 8;
        }
        crc
    }
}

/// POPCNT 位密度：雪花 vs 纯色的内容度量（加密/压缩场景的前置数）。
#[target_feature(enable = "popcnt")]
pub(crate) unsafe fn density_popcnt(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mut i = 0;
        while i + 8 <= len {
            let w = (buf.add(i) as *const u64).read_unaligned();
            let c: u64;
            asm!("popcnt {d}, {s}", d = out(reg) c, s = in(reg) w, options(nostack, preserves_flags));
            acc += c;
            i += 8;
        }
        acc
    }
}

/// AVX512VPOPCNTDQ：64B 一步的位密度，acc 用 vpaddq 常驻 zmm。
#[target_feature(enable = "avx512f,avx512vpopcntdq")]
pub(crate) unsafe fn density_vpopcntq(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::{_mm512_setzero_si512, _mm512_storeu_si512};
        let mut acc = _mm512_setzero_si512();
        let mut i = 0;
        let n = len & !63;
        while i < n {
            asm!("vmovdqu8 {v}, [{s}]", "vpopcntq {c}, {v}", "vpaddq {a}, {a}, {c}",
                v = out(zmm_reg) _, c = out(zmm_reg) _,
                a = inout(zmm_reg) acc,
                s = in(reg) buf.add(i), options(nostack));
            i += 64;
        }
        let mut out = [0u64; 8];
        _mm512_storeu_si512(out.as_mut_ptr() as *mut _, acc);
        let mut tail = 0u64;
        let mut j = n;
        while j + 8 <= len {
            tail += (buf.add(j) as *const u64).read_unaligned().count_ones() as u64;
            j += 8;
        }
        out.iter().sum::<u64>() + tail
    }
}

// ---------- 密码/GFNI/AMD 扩展：数据通路吞吐 ----------

/// AES-NI aesenc 链：帧数据当状态做一轮轮函数（密码 SIMD 通路）。
#[target_feature(enable = "aes")]
pub(crate) unsafe fn crypto_aes(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("aesenc {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// PCLMULQDQ：无进位乘法（CRC/GF 的硬件底座）。
#[target_feature(enable = "pclmulqdq")]
pub(crate) unsafe fn crypto_pclmul(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("pclmulqdq {a}, {t}, 0", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// SHA 扩展 sha1rnds4：SHA-1 轮函数（摘要加速通路）。
#[target_feature(enable = "sha")]
pub(crate) unsafe fn crypto_sha(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("sha1rnds4 {a}, {t}, 1", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// GFNI gf2p8affineqb：GF(2^8) 仿射变换（图像/密码混合通路）。
#[target_feature(enable = "gfni")]
pub(crate) unsafe fn crypto_gfni(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("gf2p8affineqb {a}, {t}, 0", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// VAES vaesenc（256 位）：宽版 AES 轮函数，需 AVX2 承载 ymm。
#[target_feature(enable = "vaes,avx2")]
pub(crate) unsafe fn crypto_vaes(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m256i;
        let mut acc: __m256i;
        asm!("vmovdqu {a}, [{p}]", a = out(ymm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !31;
        while i < n {
            let mut t: __m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("vaesenc {a}, {a}, {t}", a = inout(ymm_reg) acc, t = in(ymm_reg) t, options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        let mut o = [0u64; 4];
        asm!("vmovdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(ymm_reg) acc, options(nostack));
        o[0] ^ o[1] ^ o[2] ^ o[3]
    }
}

/// VPCLMULQDQ（256 位）：宽版无进位乘法。
#[target_feature(enable = "vpclmulqdq,avx2")]
pub(crate) unsafe fn crypto_vpclmul(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m256i;
        let mut acc: __m256i;
        asm!("vmovdqu {a}, [{p}]", a = out(ymm_reg) acc, p = in(reg) buf, options(nostack));
        let mut i = 0;
        let n = len & !31;
        while i < n {
            let mut t: __m256i;
            asm!("vmovdqu {t}, [{s}]", t = out(ymm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("vpclmulqdq {a}, {a}, {t}, 0", a = inout(ymm_reg) acc, t = in(ymm_reg) t, options(nostack));
            i += 32;
        }
        asm!("vzeroupper", options(nostack, preserves_flags));
        let mut o = [0u64; 4];
        asm!("vmovdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(ymm_reg) acc, options(nostack));
        o[0] ^ o[1] ^ o[2] ^ o[3]
    }
}

/// AMD SSE4a extrq：位段提取（AMD 128 位世代的数据通路）。
#[target_feature(enable = "sse4a")]
pub(crate) unsafe fn crypto_sse4a(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("movdqu {a}, [{p}]", a = out(xmm_reg) acc, p = in(reg) buf, options(nostack));
        let mut ctrl: __m128i;
        asm!("movdqu {c}, [{p}]", c = out(xmm_reg) ctrl, p = in(reg) buf.add(16), options(nostack));
        let mut i = 0;
        let n = len & !15;
        while i < n {
            let mut t: __m128i;
            asm!("movdqu {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("extrq {a}, {c}", a = inout(xmm_reg) acc, c = in(xmm_reg) ctrl, options(nostack));
            asm!("pxor {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 16;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

/// AMD 3DNow! pfadd：打包浮点加（整数 movq 夹层，避免浮点 Rust 类型被标量化）。
pub(crate) unsafe fn crypto_3dnow(buf: *const u8, len: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0x3f80_3f80_3f80_3f80;
        let mut i = 0;
        let n = len & !7;
        while i < n {
            let w = (buf.add(i) as *const u64).read_unaligned();
            let r: u64;
            asm!(
                "movq mm0, {a}",
                "movq mm1, {b}",
                "pfadd mm0, mm1",
                "movq {r}, mm0",
                r = out(reg) r, a = in(reg) acc, b = in(reg) w,
                out("mm0") _, out("mm1") _,
                options(nostack)
            );
            acc = r;
            i += 8;
        }
        asm!("emms", options(nostack, preserves_flags));
        acc
    }
}

/// SSE4.1 pmovzxbw：字节零扩展成字（像素拆包基线；唯一不依赖 WC 的 SSE4.1 数据原语）。
#[target_feature(enable = "sse4.1")]
pub(crate) unsafe fn extra_sse41_unpack(buf: *const u8, len: usize) -> u64 {
    unsafe {
        use core::arch::x86_64::__m128i;
        let mut acc: __m128i;
        asm!("pxor {a}, {a}", a = out(xmm_reg) acc, options(nostack));
        let mut i = 0;
        let n = len & !7;
        while i < n {
            let mut t: __m128i;
            asm!("movq {t}, [{s}]", t = out(xmm_reg) t, s = in(reg) buf.add(i), options(nostack));
            asm!("pmovzxbw {t}, {t}", t = inout(xmm_reg) t, options(nostack));
            asm!("paddw {a}, {t}", a = inout(xmm_reg) acc, t = in(xmm_reg) t, options(nostack));
            i += 8;
        }
        let mut o = [0u64; 2];
        asm!("movdqu [{p}], {a}", p = in(reg) o.as_mut_ptr(), a = in(xmm_reg) acc, options(nostack));
        o[0] ^ o[1]
    }
}

// ---------- 生成：xorshift vs 硬件 RNG ----------
pub(crate) unsafe fn rng_xorshift(dst: *mut u32, n: usize, seed: u32) {
    unsafe {
        let mut rng = seed | 1;
        for i in 0..n {
            rng ^= rng.wrapping_shl(13);
            rng ^= rng.wrapping_shr(17);
            rng ^= rng.wrapping_shl(5);
            dst.add(i).write(rng);
        }
        core::hint::black_box(dst);
    }
}

/// RDRAND：10 次重试/字，失败记数不断流（NIST 级熵，雪花屏的另一原料）。
#[target_feature(enable = "rdrand")]
pub(crate) unsafe fn rng_rdrand(dst: *mut u32, n: usize) -> u32 {
    unsafe {
        let mut fails = 0u32;
        for i in 0..n {
            let mut v = 0u32;
            let mut ok = 0u8;
            for _ in 0..10 {
                asm!("rdrand {v:e}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nostack));
                if ok == 1 {
                    break;
                }
            }
            if ok == 1 {
                dst.add(i).write(v);
            } else {
                fails += 1;
                dst.add(i).write(0x9E37_79B9);
            }
        }
        core::hint::black_box(dst);
        fails
    }
}

/// RDSEED：种子级熵，比 RDRAND 慢一个量级，轮数减半。
#[target_feature(enable = "rdseed")]
pub(crate) unsafe fn rng_rdseed(dst: *mut u32, n: usize) -> u32 {
    unsafe {
        let mut fails = 0u32;
        for i in 0..n {
            let mut v = 0u32;
            let mut ok = 0u8;
            for _ in 0..20 {
                asm!("rdseed {v:e}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nostack));
                if ok == 1 {
                    break;
                }
            }
            if ok == 1 {
                dst.add(i).write(v);
            } else {
                fails += 1;
                dst.add(i).write(0x9E37_79B9);
            }
        }
        core::hint::black_box(dst);
        fails
    }
}

// ---------- GPR 像素 helper：BMI/LZCNT（标量寄存器，无向量后端风险） ----------

/// BMI1-BEXTR：逐像素抽 R 通道（bits16..23），control=0x810 常驻寄存器。
#[target_feature(enable = "bmi1")]
pub(crate) unsafe fn gpr_bextr_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let ctrl: u32 = 16 | (8 << 8);
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("bextr {d:e}, {s:e}, {c:e}", d = out(reg) r, s = in(reg) v, c = in(reg) ctrl, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2-PEXT：聚拢每像素 bit0（mask=0x01010101）→ 4bit 位平面，位平面打包原语。
#[target_feature(enable = "bmi2")]
pub(crate) unsafe fn gpr_pext_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mask: u32 = 0x0101_0101;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("pext {d:e}, {s:e}, {m:e}", d = out(reg) r, s = in(reg) v, m = in(reg) mask, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// LZCNT：逐字前导零累加，内容稀疏度扫描。
#[target_feature(enable = "lzcnt")]
pub(crate) unsafe fn gpr_lzcnt_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("lzcnt {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI1 ANDN：dest = ~a & b。
#[target_feature(enable = "bmi1")]
pub(crate) unsafe fn gpr_andn_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("andn {d:e}, {s:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc = acc.wrapping_add(r as u64);
        }
        acc
    }
}

/// TZCNT：尾零计数（AMD 走 ABM，Intel 走 BMI1，编码同为 F3 0F BC）。
#[target_feature(enable = "bmi1")]
pub(crate) unsafe fn gpr_tzcnt_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("tzcnt {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 PDEP：按掩码分散位。
#[target_feature(enable = "bmi2")]
pub(crate) unsafe fn gpr_pdep_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let mask: u32 = 0x0101_0101;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("pdep {d:e}, {s:e}, {m:e}", d = out(reg) r, s = in(reg) v, m = in(reg) mask, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 RORX：无标志循环右移。
#[target_feature(enable = "bmi2")]
pub(crate) unsafe fn gpr_rorx_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("rorx {d:e}, {s:e}, 8", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// BMI2 BZHI：清零高位（保留低 n 位）。
#[target_feature(enable = "bmi2")]
pub(crate) unsafe fn gpr_bzhi_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        let idx: u32 = 24;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("bzhi {d:e}, {s:e}, {i:e}", d = out(reg) r, s = in(reg) v, i = in(reg) idx, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// MOVBE：字节序交换加载（端序转换通路）。
#[target_feature(enable = "movbe")]
pub(crate) unsafe fn gpr_movbe_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let r: u32;
            asm!("movbe {d:e}, [{s}]", d = out(reg) r, s = in(reg) buf.add(i), options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}

/// AMD TBM blcfill：清最低置位以下所有位。
#[target_feature(enable = "tbm")]
pub(crate) unsafe fn gpr_blcfill_sum(buf: *const u32, n: usize) -> u64 {
    unsafe {
        let mut acc: u64 = 0;
        for i in 0..n {
            let v = buf.add(i).read_unaligned();
            let r: u32;
            asm!("blcfill {d:e}, {s:e}", d = out(reg) r, s = in(reg) v, options(nostack, preserves_flags));
            acc += r as u64;
        }
        acc
    }
}
