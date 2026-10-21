//! AVC(H.264) 解码探针：内嵌小样片在固件环境实测 `rusty_h264-decoder`。
//!
//! 轮子结论（根 AGENTS.md「轮子优先」，2026-10-20 host 实测，探针在 `/tmp/h264probe`）：
//! - 搜过 crates.io H.264 解码：`rusty_h264-decoder 0.16.1`（28k 下载、两天前刚发版，
//!   rust 1.80 < 本仓 MSRV 1.92、BSD-2-Clause、`forbid(unsafe)` 核心、
//!   no_std 经 `alloc` + `libm`，SIMD 经可选 `asm` 纯 Rust 可移植内核）。
//!   不开上游 `global-alloc`：它会装轮子自带的进程全局分配器，与 uefi 的
//!   `#[global_allocator]` 撞车（`cargo check` 实锤 duplicate），
//!   上游文档明示自带分配器的下游用 `default-features = false`。
//!   树里现有的 `less-avc` 是*编码器*（1.3k 行、2023 年后未动、默认 std），不能解码；
//!   `openh264` 系是 C 绑定，UEFI 无 C 工具链且非 no_std 友好，故都不用。
//! - host 实测：Baseline/High 双档 320x240 样片与 ffmpeg 逐字节一致，
//!   乱字节/截断流优雅 0 帧无 panic，mp4（shiguredo_mp4 取样 + avcC 拼 Annex-B）
//!   整链与直解一致；`asm` 开后 720p 上 census 全进内核（wide AVX2 + base SSE2，
//!   scalar 零调用），baseline 1.55×、main/CABAC 1.23×（i5-10400）。
//!   本项只验“轮子在 UEFI 里能跑”，全规格一致性以 host 探针为准。
//! - 固件形态：`[accel]` 单比特开关（`scalar`=关/其余=开；开则解码取轮子
//!   max，SSE2 保底、AVX2 看固件门——no_std 下轮内 have 恒最高档，真门是开机
//!   `resolve_accel` 按 `copy_width()` 推下来的 `set_simd_enabled`，轮内无
//!   第二套 CPUID/XCR0 探；首解前这里再幂等 `ensure_xsave_enabled` 一次，
//!   防 OVMF 默认 CR4.OSXSAVE=0 下 XGETBV 的 `#UD`）。
//!   accel 箱 vendor 在 `third_party/rusty_h264-accel`（`std` 默认开保上游行为，
//!   固件走无 `std` 形，经根 `[patch.crates-io]` 截流，见该目录 VENDOR.md）。
//!   前代两道门（std 撞车 E0152 / 裸 flags 下 LLVM ICE）已由 vendor 自 port +
//!   本仓 flags 消解（`cargo check` 能过≠能用的教训仍记：终链才算数）。
//!
//! 样片：`data/avc_probe_baseline.264`（ffmpeg testsrc 128x96×4 帧 baseline 无 B 帧，
//! 约 4KB，`include_bytes!` 编进固件，免 U 盘文件依赖）。
//! 断言：帧数/几何/全帧 Luma wrapping 和 + `avc: PASS` 行供 QEMU 矩阵断言。

use alloc::vec::Vec;

use crate::emit;
use crate::error::{NyaStatus, Result};
use crate::graphics::Screen;

/// 内嵌样片（Annex-B，SPS/PPS 在带内）。
const SAMPLE: &[u8] = include_bytes!("data/avc_probe_baseline.264");
/// 期望值（host 探针实测：4 帧 128x96，全帧 Luma（仅 Y 平面）wrapping 和；
/// 注意 host 原始输出是 YUV 逐帧拼帧，混算会把色度带进来，固件侧只加 Y）。
const EXPECT_FRAMES: usize = 4;
const EXPECT_W: usize = 128;
const EXPECT_H: usize = 96;
const EXPECT_LUMA_SUM: u64 = 6078137;

fn fail(msg: &str) -> NyaStatus {
    NyaStatus::_Debug(alloc::string::String::from(msg))
}

pub fn run(screen: &mut Screen) -> Result {
    emit!(
        screen,
        "=== AVC(H.264) decode probe (rusty_h264-decoder) ==="
    );
    // 首解前幂等保一次 XSAVE（`resolve_accel` 开机已做；这里防首解跑到它前面；
    // hevc 同款。AVX2 核跑前过 XCR0 门，OVMF 默认 OSXSAVE=0 下 XGETBV 会 #UD）。
    crate::diag::common::ensure_xsave_enabled();
    let frames: Vec<_> = rusty_h264_decoder::Decoder::new()
        .decode_stream(SAMPLE)
        .map_err(|e| fail(&alloc::format!("avc: decode error: {:?}", e)))?;
    emit!(screen, "avc: decoded {} frames", frames.len());
    if frames.len() != EXPECT_FRAMES {
        return Err(fail(&alloc::format!(
            "avc: frame count {} != {}",
            frames.len(),
            EXPECT_FRAMES
        )));
    }
    let mut sum = 0u64;
    for (i, f) in frames.iter().enumerate() {
        if f.width != EXPECT_W || f.height != EXPECT_H {
            return Err(fail(&alloc::format!(
                "avc: frame {} geometry {}x{} != {}x{}",
                i,
                f.width,
                f.height,
                EXPECT_W,
                EXPECT_H
            )));
        }
        for b in f.as_planes().y {
            sum = sum.wrapping_add(*b as u64);
        }
    }
    emit!(
        screen,
        "avc: {}x{} luma_sum={} (expect {})",
        EXPECT_W,
        EXPECT_H,
        sum,
        EXPECT_LUMA_SUM
    );
    if sum != EXPECT_LUMA_SUM {
        return Err(fail("avc: luma checksum mismatch"));
    }
    emit!(screen, "avc: PASS");
    Ok(())
}
