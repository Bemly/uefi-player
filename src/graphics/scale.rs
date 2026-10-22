//! `graphics::scale`：长宽比适配 + 双线性缩放（固件侧自研，不引入新依赖）。
//!
//! 轮子结论：`image` 等 crates 是 `std` 重依赖，不适合 `no_std` 固件；且本层直接
//! 操作 `BltPixel`（BGRA，`blue/green/red` 三字节 + 保留字节）切片，定点查表即可，
//! 无需通用像素格式抽象，故手写（中文注释允许，见 AGENTS）。
//!
//! - [`fit_rect`]：aspect-fit 居中，返回 `(ox, oy, fw, fh)`（全 `u32`，零尺寸守卫）。
//! - [`scale_bilinear`]：`src(sw×sh)` → `dst(dw×dh)` 双线性，定点 16.16 步进 +
//!   8 位权重，边缘 clamp，零尺寸/切片不足直接返回（不 panic，不断播放流程）。
//!
//! SIMD 说明（读 `src/diag/gop_simd/kernels.rs` 后结论）：本仓 SIMD 约定是 `asm!`
//! 显式向量寄存器（`in(xmm_reg)`/`in(ymm_reg)`/`in(zmm_reg)` + `target_feature` 门控，
//! AVX 系收尾 `vzeroupper`，防 LLVM 标量化）。双线性是二维 gather + 加权 lerp，
//! 不是 `fill/copy` 那种连续搬运，尚无“行核 + 列核”定点展开；X128/Y256/Z512
//! 定点核留待可 clean 时再加。当前所有 `CopyWidth` 档位统一走标量正确实现，
//! 分发点已按位宽预留（phase 2 只填核不动签名）；本文件即 bit-exact 基准。
//!
//! 多分辨率 perf 说明（8 格式调研结论）：小帧放大（如 H.263 128x96 → fit
//! 1440x1080 ≈ 1.55M 目标像素）时本核是主瓶颈（每目标像素 4 源加载 +
//! 6 次 `lerp8`，全标量，与 `CopyWidth` 无关；位宽只管最终搬运
//! `blit_rows_direct`/`copy_block`）。本函数做三处零语义优化（bit-exact 不变，
//! 映射 `sx = x*(sw-1)/(dw-1)` 角对齐、`lerp8` 四舍五入、`clamp` 边复制全保留）：
//! (1) `fx` 行内累加（`u32` 加代替每像素 `u64` 乘，`x*step` 与累加恒等，无溢出）；
//! (2) 行基址指针提升 + `wx == 0` / `wy == 0` 恒等捷径（`lerp8(a,b,0) == a`
//! 恒成立，见 `lerp8` 式，跳过的乘加结果与全量路径逐位一致）；
//! (3) 源/目标行指针裸访问（调用方已保 `len`，`debug_assert` 守卫，release 免界检）。
//! `1080p` native 走 `upload_present_native` 零缩放，本路零触达、零回归。
//!
//! H.263 letterbox vs presentation-time 互补（见 `present` 头）：本核只管
//! 给定源→目标的像素映射，不管“源几何从哪来”（encode-time 已定）。

use crate::config::{CopyWidth, DisplayAlign, DisplayFit};
use uefi::proto::console::gop::BltPixel;

// v2 补充（`rect`/`place_axis`/`clip_blit`，`[display]` fit 五档 + align 五档）：
// `rect(sw,sh,dw,dh,fit,align)->(ox,oy,fw,fh)` 为总摆位（`fw,fh` 缩放后图尺寸，
// cover 可能大于屏；`ox,oy` 屏上目标原点，放不下的轴恒 0，裁剪起点另取
// `place_axis` 源偏移，按同 `align` 锚定）；stretch 忽略 align；fit 黑边锚定
// （默认 center 与 `fit_rect` 同值）；cover=aspect-fill 后按 align 裁满屏；
// scale_down=放得下走 native 1:1+align，否则走 fit；native=恒 1:1+align
// （v1 左上→v2 默认居中，见 `present` 头）。上传侧按窗推（Blt 源窗/目标窗、
// 直写影子行列双限），不写屏外、不读源外（`native+oversize` 如 16CIF H.263
// 直写路旧 OOB 即靠本层窗裁修好）。

/// aspect-fit 居中：把 `sw×sh` 源等比放进 `dw×dh` 目标，返回 `(ox, oy, fw, fh)`。
///
/// 任一输入为 0 返回全 0（调用方跳过 blt，不断流程）；非零输入保证 `fw/fh ≥ 1`。
pub(crate) fn fit_rect(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0, 0, 0);
    }
    // `u64` 中间值防溢出：源更宽（sw/sh > dw/dh）则定宽算高，否则定高算宽。
    let (fw, fh) = if (sw as u64) * (dh as u64) > (dw as u64) * (sh as u64) {
        let fh = ((sh as u64) * (dw as u64) / (sw as u64)) as u32;
        (dw, fh.max(1).min(dh))
    } else {
        let fw = ((sw as u64) * (dh as u64) / (sh as u64)) as u32;
        (fw.max(1).min(dw), dh)
    };
    let ox = (dw - fw) / 2;
    let oy = (dh - fh) / 2;
    (ox, oy, fw, fh)
}

/// 单轴摆位裁剪：`scaled` 为该轴缩放后长度，`screen` 为屏长；`lo` 贴左/上
/// （`Left`/`Top`），`hi` 贴右/下（`Right`/`Bottom`），皆无即居中。
/// 返回 `(dest, src, vis)`：放得下时 `dest` 按锚摆、`src`=0；放不下时 `dest`=0、
/// `src` 为裁剪窗起点（按同锚）。恒有 `dest+vis<=screen` 且 `src+vis<=scaled`。
pub(crate) fn place_axis(scaled: u32, screen: u32, lo: bool, hi: bool) -> (u32, u32, u32) {
    if scaled <= screen {
        let dest = if lo {
            0
        } else if hi {
            screen - scaled
        } else {
            (screen - scaled) / 2
        };
        (dest, 0, scaled)
    } else {
        let over = scaled - screen;
        let src = if lo {
            0
        } else if hi {
            over
        } else {
            over / 2
        };
        (0, src, screen)
    }
}

/// aspect-fill 尺寸（cover 用）：按大边等比放大（ceil 取整保证两轴都不小于屏；
/// `u64` 中间值防溢出），任一输入为 0 返回 `(0, 0)`。
fn cover_dims(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0);
    }
    // 源相对更宽（sw/sh > dw/dh）则定高算宽（ceil），否则定宽算高。
    if (sw as u64) * (dh as u64) > (dw as u64) * (sh as u64) {
        let fw = ((sw as u64) * (dh as u64)).div_ceil(sh as u64);
        (fw.min(u32::MAX as u64) as u32, dh)
    } else {
        let fh = ((sh as u64) * (dw as u64)).div_ceil(sw as u64);
        (dw, fh.min(u32::MAX as u64) as u32)
    }
}

/// v2 总摆位：`(sw,sh)` 源→`(dw,dh)` 屏，按 `fit`+`align` 返回 `(ox, oy, fw, fh)`。
/// `fw,fh` 为缩放后图尺寸（cover 可能大于屏）；`ox,oy` 为屏上目标原点（放不下的
/// 轴恒 0，该轴裁剪起点取 [`place_axis`] 源偏移，按同 `align` 锚定）。
/// stretch 忽略 align；fit 黑边锚定（默认 center 与 [`fit_rect`] 同值）；
/// cover=aspect-fill 后按 align 裁满屏；scale_down 放得下走 native 1:1+align，
/// 否则走 fit；native 恒 1:1+align（v1 左上→v2 默认居中，见 `present` 头）。
/// 任一输入为 0 返回全 0（调用方跳过 push，不断流程）。
pub(crate) fn rect(
    sw: u32,
    sh: u32,
    dw: u32,
    dh: u32,
    fit: DisplayFit,
    align: DisplayAlign,
) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0, 0, 0);
    }
    let (lo_x, hi_x) = (align == DisplayAlign::Left, align == DisplayAlign::Right);
    let (lo_y, hi_y) = (align == DisplayAlign::Top, align == DisplayAlign::Bottom);
    match fit {
        DisplayFit::Stretch => (0, 0, dw, dh),
        DisplayFit::Fit => {
            let (_, _, fw, fh) = fit_rect(sw, sh, dw, dh);
            let (ox, _, _) = place_axis(fw, dw, lo_x, hi_x);
            let (oy, _, _) = place_axis(fh, dh, lo_y, hi_y);
            (ox, oy, fw, fh)
        }
        DisplayFit::Cover => {
            let (fw, fh) = cover_dims(sw, sh, dw, dh);
            (0, 0, fw, fh)
        }
        DisplayFit::ScaleDown => {
            if sw <= dw && sh <= dh {
                let (ox, _, _) = place_axis(sw, dw, lo_x, hi_x);
                let (oy, _, _) = place_axis(sh, dh, lo_y, hi_y);
                (ox, oy, sw, sh)
            } else {
                let (_, _, fw, fh) = fit_rect(sw, sh, dw, dh);
                let (ox, _, _) = place_axis(fw, dw, lo_x, hi_x);
                let (oy, _, _) = place_axis(fh, dh, lo_y, hi_y);
                (ox, oy, fw, fh)
            }
        }
        DisplayFit::Native => {
            let (ox, _, _) = place_axis(sw, dw, lo_x, hi_x);
            let (oy, _, _) = place_axis(sh, dh, lo_y, hi_y);
            (ox, oy, sw, sh)
        }
    }
}

/// 1:1 小窗裁剪：`fw×fh` 源（==源切片尺寸）按 `(ox,oy)` 画进 `dw×dh` 屏，返回
/// `(dx,dy,sx,sy,vw,vh)`（目标窗+源窗+可见尺寸），恒有 `dx+vw<=dw`、`dy+vh<=dh`、
/// `sx+vw<=fw`、`sy+vh<=fh`。完全越界/零尺寸回 `None`（调用方跳过，不断流程）。
/// fit/scale_down-小/native-恰好的窗走这里（恒等通过，零代价）；cover/oversize
/// 的带偏移源窗走 [`place_axis`] 双轴（见 `graphics.rs`）。
pub(crate) fn clip_blit(
    ox: u32,
    oy: u32,
    fw: u32,
    fh: u32,
    dw: u32,
    dh: u32,
) -> Option<(u32, u32, u32, u32, u32, u32)> {
    if fw == 0 || fh == 0 || dw == 0 || dh == 0 {
        return None;
    }
    if ox >= dw || oy >= dh {
        return None;
    }
    let vw = fw.min(dw - ox);
    let vh = fh.min(dh - oy);
    if vw == 0 || vh == 0 {
        return None;
    }
    Some((ox, oy, 0, 0, vw, vh))
}

/// align→双轴锚定位（`place_axis` 的 `lo/hi` 口径）：`(lo_x, hi_x, lo_y, hi_y)`。
/// `Left` 只置横左，`Right` 只置横右，`Top` 只置纵上，`Bottom` 只置纵下，
/// `Center`/不管的轴恒居中（调用方 `present::align()` 取值，类型推导免 import）。
pub(crate) fn align_sides(align: DisplayAlign) -> (bool, bool, bool, bool) {
    (
        align == DisplayAlign::Left,
        align == DisplayAlign::Right,
        align == DisplayAlign::Top,
        align == DisplayAlign::Bottom,
    )
}

/// 双线性缩放：`src`（`sw×sh`）→ `dst`（`dw×dh`），按 `width` 位宽分发。
///
/// 当前为分发 stub：全部档位走标量正确路径（SIMD 核待补，见文件头）。
/// 切片长度不足（`< sw*sh` / `< dw*dh`）或任一维度为 0 时直接返回，不写半帧。
#[allow(clippy::match_same_arms)]
pub(crate) fn scale_bilinear(
    src: &[BltPixel],
    sw: usize,
    sh: usize,
    dst: &mut [BltPixel],
    dw: usize,
    dh: usize,
    width: CopyWidth,
) {
    match width {
        // 标量/U64 与 X128 同走标量核（连续搬运可用 128 位，gather-lerp 尚无核）。
        CopyWidth::U64 | CopyWidth::X128 | CopyWidth::V128 => {
            scale_bilinear_scalar(src, sw, sh, dst, dw, dh);
        }
        // Y256/Z512 定点行核待补（见文件头），先走标量保证 bit-exact。
        CopyWidth::Y256 => scale_bilinear_scalar(src, sw, sh, dst, dw, dh),
        CopyWidth::Z512 => scale_bilinear_scalar(src, sw, sh, dst, dw, dh),
    }
}

/// 8 位权重单通道 lerp：`w` 为 `b` 的权重（0..=255），四舍五入。
#[inline(always)]
fn lerp8(a: u8, b: u8, w: u32) -> u8 {
    (((a as u32) * (256 - w) + (b as u32) * w + 128) >> 8) as u8
}

/// 标量双线性核（bit-exact 基准）：16.16 定点步进，每轴一次除法、每像素只乘加。
///
/// 映射取角对齐 `sx = x*(sw-1)/(dw-1)`（`dw == 1` 时恒取 0 列，`sw == 1` 时无水平
/// 插值，垂直同理），`x1/y1` clamp 到边界，等价于边缘复制。
fn scale_bilinear_scalar(
    src: &[BltPixel],
    sw: usize,
    sh: usize,
    dst: &mut [BltPixel],
    dw: usize,
    dh: usize,
) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let need_src = sw.saturating_mul(sh);
    let need_dst = dw.saturating_mul(dh);
    if src.len() < need_src || dst.len() < need_dst {
        return;
    }
    // 等尺寸即逐像素拷贝（与双线性恒等映射 bit 一致，走更快路径）。
    if sw == dw && sh == dh {
        dst[..need_dst].copy_from_slice(&src[..need_src]);
        return;
    }
    // 16.16 步进：`(sw-1)<<16/(dw-1)`，`dw == 1` 时步进 0（单列取首列）。
    let step_x: u32 = if dw > 1 {
        (((sw as u64).saturating_sub(1) << 16) / (dw as u64 - 1)) as u32
    } else {
        0
    };
    let step_y: u32 = if dh > 1 {
        (((sh as u64).saturating_sub(1) << 16) / (dh as u64 - 1)) as u32
    } else {
        0
    };
    debug_assert!(sw >= 1 && sh >= 1);
    // 行基址裸指针（长度已保，行窗恒在界内；`sw/sh >= 1` 故 `sh - 1` 无下溢）。
    let src_ptr = src.as_ptr();
    let dst_ptr = dst.as_mut_ptr();
    let swm1 = sw - 1;
    let shm1 = sh - 1;
    let mut fy: u64 = 0;
    for y in 0..dh {
        let y0 = ((fy >> 16) as usize).min(shm1);
        let y1 = (y0 + 1).min(shm1);
        // 8 位权重取小数高 8 位（0..=255），整数坐标时恒 0。
        let wy = ((fy & 0xFFFF) >> 8) as u32;
        let row0 = (y0 * sw) as isize;
        let row1 = (y1 * sw) as isize;
        let drow = (y * dw) as isize;
        // `fx` 行内累加：`x * step_x` 与 `+= step_x` 恒等（`u64` 无溢出：
        // `fx <= (sw-1) << 16`，源宽解码器上限远小于 `2^48`）。
        let mut fx: u64 = 0;
        for x in 0..dw {
            let x0 = ((fx >> 16) as usize).min(swm1);
            let x1 = (x0 + 1).min(swm1);
            let wx = ((fx & 0xFFFF) >> 8) as u32;
            fx += step_x as u64;
            // SAFETY：`row0 + x0/x1`、`row1 + x0/x1` 恒 `< sw*sh <= src.len()`，
            // `drow + x` 恒 `< dw*dh <= dst.len()`（入口已验长度，窗经 clamp）。
            unsafe {
                let p00 = *src_ptr.offset(row0 + x0 as isize);
                if wx == 0 {
                    // 水平恒等：`lerp8(a,b,0) == a`，上下行直接取左像素。
                    let p10 = *src_ptr.offset(row1 + x0 as isize);
                    if wy == 0 {
                        *dst_ptr.offset(drow + x as isize) = p00;
                    } else {
                        *dst_ptr.offset(drow + x as isize) = BltPixel::new(
                            lerp8(p00.red, p10.red, wy),
                            lerp8(p00.green, p10.green, wy),
                            lerp8(p00.blue, p10.blue, wy),
                        );
                    }
                } else if wy == 0 {
                    // 垂直恒等：终值即水平行结果，省下行 lerp。
                    let p01 = *src_ptr.offset(row0 + x1 as isize);
                    *dst_ptr.offset(drow + x as isize) = BltPixel::new(
                        lerp8(p00.red, p01.red, wx),
                        lerp8(p00.green, p01.green, wx),
                        lerp8(p00.blue, p01.blue, wx),
                    );
                } else {
                    let p01 = *src_ptr.offset(row0 + x1 as isize);
                    let p10 = *src_ptr.offset(row1 + x0 as isize);
                    let p11 = *src_ptr.offset(row1 + x1 as isize);
                    // 先水平后垂直（`BltPixel::new(r, g, b)` 形参与内存 BGRA 序注意区分）。
                    let top_b = lerp8(p00.blue, p01.blue, wx);
                    let top_g = lerp8(p00.green, p01.green, wx);
                    let top_r = lerp8(p00.red, p01.red, wx);
                    let bot_b = lerp8(p10.blue, p11.blue, wx);
                    let bot_g = lerp8(p10.green, p11.green, wx);
                    let bot_r = lerp8(p10.red, p11.red, wx);
                    *dst_ptr.offset(drow + x as isize) = BltPixel::new(
                        lerp8(top_r, bot_r, wy),
                        lerp8(top_g, bot_g, wy),
                        lerp8(top_b, bot_b, wy),
                    );
                }
            }
        }
        fy += step_y as u64;
    }
}
