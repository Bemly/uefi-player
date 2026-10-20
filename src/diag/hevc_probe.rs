//! HEVC(H.265) 解码探针：内嵌小样片在固件环境实测 vendor 的 `rusty_h265`。
//!
//! 轮子结论（根 AGENTS.md「轮子优先」，2026-10-21 host 实测，探针在 `/tmp/h265probe`）：
//! - 上游 0.6.2 尚无 no_std 分层（`std::Arc/VecDeque/原子量/env` 无特性门），
//!   本仓 vendor 到 `third_party/rusty_h265(+accel)` 后自 port 标量路
//!   （`std` 特性默认开保上游行为；`default-features=false` 即 no_std 标量），
//!   详见该目录 VENDOR.md；纯整数运算，连 `libm` 都不要。
//! - host 实测：320x240 Main 样片与 ffmpeg 逐字节一致，标量/SIMD 双路一致；
//!   本项只验“轮子在 UEFI 里能跑”，全规格一致性以 host 探针为准。
//! - 固件形态：`[accel] level` 单比特开关（`scalar`=关/其余=开；开则解码取
//!   轮子上限，SSE2 保底、AVX2 看 CPUID/XCR0，开机 `resolve_accel` 已按需开过
//!   XSAVE，首解前这里再幂等保一次，防 OVMF 默认 CR4.OSXSAVE=0 下 XGETBV 的
//!   `#UD`；轮内无第二套检测，见 VENDOR.md）。双胞胎与标量路 bit-exact，
//!   校验和口径（仅 Y 平面 wrapping 和）与标量路一致。
//!
//! 样片：`data/hevc_probe_main.265`（ffmpeg testsrc 320x240×10 帧 Main 无 B 帧，
//! 约 8KB，`include_bytes!` 编进固件，免 U 盘文件依赖）。
//! 断言：帧数/几何/全帧 Luma wrapping 和 + `hevc: PASS` 行供 QEMU 矩阵断言；
//! 随后按 10fps 循环播首屏（任意键停，肉眼确认用）。

use alloc::vec::Vec;

use crate::diag::common::poll_key;
use crate::emit;
use crate::error::{NyaStatus, Result};
use crate::graphics::{Screen, calibrate_tps, tsc};

/// 内嵌样片（Annex-B，VPS/SPS/PPS 在带内）。
const SAMPLE: &[u8] = include_bytes!("data/hevc_probe_main.265");
/// 期望值（host 探针实测：10 帧 320x240，全帧 Luma（仅 Y 平面）wrapping 和；
/// 标量路/SIMD 路/ffmpeg 三方一致，固件侧只加 Y）。
const EXPECT_FRAMES: usize = 10;
const EXPECT_W: usize = 320;
const EXPECT_H: usize = 240;
const EXPECT_LUMA_SUM: u64 = 95674696;
/// 演示循环帧率。
const SHOW_FPS: u64 = 10;

fn fail(msg: &str) -> NyaStatus {
    NyaStatus::_Debug(alloc::string::String::from(msg))
}

pub fn run(screen: &mut Screen) -> Result {
    emit!(screen, "=== HEVC(H.265) decode probe (rusty_h265) ===");
    // 首解前幂等保一次 XSAVE（`resolve_accel` 开机已做；这里防首解跑到它前面）。
    crate::diag::common::ensure_xsave_enabled();
    let mut dec = rusty_h265::Decoder::new();
    dec.push_annexb(SAMPLE, None)
        .map_err(|e| fail(&alloc::format!("hevc: push error: {:?}", e)))?;
    dec.flush();
    let mut frames = Vec::new();
    loop {
        match dec.next_frame() {
            Ok(f) => frames.push(f),
            Err(rusty_h265::Error::Eof) | Err(rusty_h265::Error::Again) => break,
            Err(e) => return Err(fail(&alloc::format!("hevc: decode error: {:?}", e))),
        }
    }
    emit!(screen, "hevc: decoded {} frames", frames.len());
    if frames.len() != EXPECT_FRAMES {
        return Err(fail(&alloc::format!(
            "hevc: frame count {} != {}",
            frames.len(),
            EXPECT_FRAMES
        )));
    }
    let mut sum = 0u64;
    let mut yuv = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        if f.width != EXPECT_W || f.height != EXPECT_H {
            return Err(fail(&alloc::format!(
                "hevc: frame {} geometry {}x{} != {}x{}",
                i,
                f.width,
                f.height,
                EXPECT_W,
                EXPECT_H
            )));
        }
        yuv.clear();
        f.write_yuv(&mut yuv);
        let ybytes = EXPECT_W * EXPECT_H;
        if yuv.len() < ybytes {
            return Err(fail("hevc: short YUV write"));
        }
        for b in &yuv[..ybytes] {
            sum = sum.wrapping_add(*b as u64);
        }
    }
    emit!(
        screen,
        "hevc: {}x{} luma_sum={} (expect {})",
        EXPECT_W,
        EXPECT_H,
        sum,
        EXPECT_LUMA_SUM
    );
    if sum != EXPECT_LUMA_SUM {
        return Err(fail("hevc: luma checksum mismatch"));
    }
    emit!(screen, "hevc: PASS");
    play_loop(screen, &frames);
    Ok(())
}

/// 演示循环：按 SHOW_FPS 轮播全帧，任意键停（肉眼确认用）。
/// 串口已打出 PASS，矩阵不断言这里；交互 QEMU 里看动画靠它。
fn play_loop(screen: &mut Screen, frames: &[rusty_h265::Frame]) {
    let tps = calibrate_tps().max(1);
    let budget = tps / SHOW_FPS.max(1);
    let mut yuv = Vec::new();
    let mut i = 0usize;
    let mut last = tsc();
    log::warn!("hevc: playing {} frames, any key stops", frames.len());
    loop {
        if poll_key().is_some() {
            log::warn!("hevc: stopped");
            return;
        }
        if tsc().saturating_sub(last) >= budget {
            last = tsc();
            yuv.clear();
            frames[i].write_yuv(&mut yuv);
            show_frame(screen, &yuv, EXPECT_W, EXPECT_H);
            i = (i + 1) % frames.len();
        }
    }
}

/// 单帧上屏（整数量化 BT.601 全范围 + 最近邻整数放大居中）。
fn show_frame(screen: &mut Screen, yuv: &[u8], w: usize, h: usize) {
    use uefi::proto::console::gop::BltPixel;
    let (mw, mh) = screen.get_gop().current_mode_info().resolution();
    let (mw, mh) = (mw as usize, mh as usize);
    if yuv.len() < w * h * 3 / 2 {
        return;
    }
    let scale = (mw / w).min(mh / h).max(1);
    let (dw, dh) = (w * scale, h * scale);
    let (ox, oy) = ((mw - dw) / 2, (mh - dh) / 2);
    let mut fb: Vec<BltPixel> = Vec::new();
    if fb.try_reserve_exact(mw * mh).is_err() {
        return;
    }
    fb.extend(core::iter::repeat(BltPixel::new(0, 0, 0)).take(mw * mh));
    let (ypl, rest) = yuv.split_at(w * h);
    let (upl, vpl) = rest.split_at(w / 2 * (h / 2));
    for dy in 0..dh {
        let sy = dy / scale;
        for dx in 0..dw {
            let sx = dx / scale;
            let y = ypl[sy * w + sx] as i32;
            let u = upl[sy / 2 * (w / 2) + sx / 2] as i32 - 128;
            let v = vpl[sy / 2 * (w / 2) + sx / 2] as i32 - 128;
            let r = (y + ((359 * v) >> 8)).clamp(0, 255) as u8;
            let g = (y - ((88 * u + 183 * v) >> 8)).clamp(0, 255) as u8;
            let b = (y + ((454 * u) >> 8)).clamp(0, 255) as u8;
            fb[(oy + dy) * mw + ox + dx] = BltPixel::new(r, g, b);
        }
    }
    if screen.upload_frame(mw, mh, &fb).is_err() {
        log::warn!("hevc: show failed");
    }
}
