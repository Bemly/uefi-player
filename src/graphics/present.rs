//! `graphics::present`：`[display]` presentation-time 适配（固件侧自研，不引入新依赖）。
//!
//! 轮子结论：`image` 等 crates 是 `std` 重依赖，不适合 `no_std` 固件；本层直接
//! 操作 `BltPixel` 切片 + `scale::fit_rect/scale_bilinear` 定点核，无需通用像素
//! 格式抽象，故手写（中文注释允许，见 AGENTS；`no_std + alloc only`，零新依赖）。
//!
//! - H.263 letterbox（encode-time，kept）vs 本 presentation-time policy 互补、
//!   不冗余：前者是解码器把 128×96 等小画面按 ITU 裁剪/补边成规格帧（`Clip`
//!   内已定几何，见 `decoder::eager::from_h263_bytes`）；后者是上屏时把已定
//!   几何的 `Clip` 帧按 `[display] fit/bar/align` 摆进 GOP 屏（stretch 全屏拉伸 /
//!   fit 等比 + bar 底 + align 锚定 / cover 等比放大裁满 / scale_down 收缩才缩 /
//!   native 1:1 + align 锚定）。两层各管一段，不互相替代。
//! - 运行时缓存：`src/config/*` 冻结不可动，`Config::load` 掉 `cfg` 后无法再取
//!   display 值，故本模块首次上屏/打标时 `config::parse()` 重读一次 player.toml
//!   进原子量（单线程 BSP 期，无竞争；未知串回默认 + warn 由 `display.rs` 的
//!   Deserialize 访客已打，此处重读会再打一行，属已知重复，不断流程）。
//!
//! 串口 marker（矩阵卡点用，每 clip 起播记一笔，见 `video::loops`）：
//!
//! ```text
//! display: fit=<fit> bar=<hex> align=<align> src=<sw>x<sh> dst=<fw>x<fh>@<ox>,<oy> scr=<W>x<H>
//! ```
//!
//! `fit=.. bar=..` 相邻在前（v3 起 `bar=<rrggbb>` hex，如 `bar=000000`/`bar=102040`；
//! v2 的 `bar=black` 字面已改 hex `bar=000000`）；
//! fit/align 全小写 `file_str`；cover 时 `fw,fh` 为放大后图尺寸（可能大于屏，
//! 可见窗为屏，裁剪锚点见 `align`）；native 行为变更（v2）：v1 恒左上 0,0，
//! v2 按 align 摆（默认 center 居中，源大于屏时居中裁剪；`docs/config.md` 有注记）。
//!
//! SIMD 说明：缩放核即 `scale.rs`（标量正确 + X128/Y256/Z512 分发 stub，
//! `asm!` 显式向量寄存器约定见 `diag/gop_simd/kernels.rs`）；本模块只做
//! 策略 + 底色 + 几何，不碰向量核。

use core::sync::atomic::{AtomicU8, Ordering};

use uefi::proto::console::gop::BltPixel;

use crate::config::{BarColor, DisplayAlign, DisplayFit};

use super::scale::rect;

/// `DisplayFit` 判别值缓存（`as_u8` 口径：0 = stretch，1 = fit（默认），2 = cover，
/// 3 = scale_down，4 = native）。
static FIT: AtomicU8 = AtomicU8::new(1);
/// `BarColor` RGB 缓存（自由 RGB，单 `u8` 装不下故三分量；默认 `BLACK`=(0,0,0)）。
static BAR_R: AtomicU8 = AtomicU8::new(0);
static BAR_G: AtomicU8 = AtomicU8::new(0);
static BAR_B: AtomicU8 = AtomicU8::new(0);
/// `DisplayAlign` 判别值缓存（`as_u8` 口径：0 = center（默认），1 = left，2 = right，
/// 3 = top，4 = bottom）。
static ALIGN: AtomicU8 = AtomicU8::new(0);
/// 缓存已装填（0 = 未读盘，1 = 已读；BSP 单线程装填一次，AP 不碰）。
static READY: AtomicU8 = AtomicU8::new(0);

/// 首次调用时重读一次 player.toml 装填 fit/bar/align（`src/config` 冻结的代价；
/// 已装填则 no-op，保证每帧调用零 I/O）。
pub(crate) fn ensure_loaded() {
    if READY.load(Ordering::Relaxed) != 0 {
        return;
    }
    let (cfg, _) = crate::config::parse();
    FIT.store(cfg.display.fit.as_u8(), Ordering::Relaxed);
    let bar = cfg.display.bar;
    BAR_R.store(bar.r, Ordering::Relaxed);
    BAR_G.store(bar.g, Ordering::Relaxed);
    BAR_B.store(bar.b, Ordering::Relaxed);
    ALIGN.store(cfg.display.align.as_u8(), Ordering::Relaxed);
    READY.store(1, Ordering::Relaxed);
}

/// 生效 fit（默认 `Fit`；显式档位超范围已由 `from_u8` 钳回默认）。
pub(crate) fn fit() -> DisplayFit {
    ensure_loaded();
    DisplayFit::from_u8(FIT.load(Ordering::Relaxed))
}

/// 生效 bar（默认 `BLACK`）。
pub(crate) fn bar() -> BarColor {
    ensure_loaded();
    BarColor {
        r: BAR_R.load(Ordering::Relaxed),
        g: BAR_G.load(Ordering::Relaxed),
        b: BAR_B.load(Ordering::Relaxed),
    }
}

/// 生效 align（默认 `Center`；显式档位超范围已由 `from_u8` 钳回默认）。
pub(crate) fn align() -> DisplayAlign {
    ensure_loaded();
    DisplayAlign::from_u8(ALIGN.load(Ordering::Relaxed))
}

/// 黑边底色（`BltPixel::new(r,g,b)` 形参顺序；`graphics.rs` 的 `VideoFill` 同口径；
/// v3 自由 RGB，预设见 `BarColor::{BLACK,WHITE,GRAY,NAVY}`）。
pub(crate) fn bar_pixel() -> BltPixel {
    let b = bar();
    BltPixel::new(b.r, b.g, b.b)
}

/// 目标几何（无日志版，上屏每帧调）：`scale::rect` 按生效 fit+align 摆位；
/// stretch 全屏，fit/scale_down-大等比 + align 锚黑边，cover 放大裁满，
/// scale_down-小/native 1:1 + align（默认居中；源大于屏时裁剪窗亦按 align）。
/// 任一输入为 0 返回全 0（调用方跳过 push，不断流程）。
pub(crate) fn geometry(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    rect(sw, sh, dw, dh, fit(), align())
}

/// 每 clip 起播记一笔（`log::warn!`，QEMU 矩阵卡点用）并返回目标几何。
/// 调用点：单线程环入口 + 切片推进后（见 `video::loops`）；mp/dynamic 入口
/// 也记（v1 多核路视觉仍 native，见各文件头，marker 只述配置意图）。
pub(crate) fn note_clip_start(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    ensure_loaded();
    let f = DisplayFit::from_u8(FIT.load(Ordering::Relaxed));
    let b = bar();
    let a = DisplayAlign::from_u8(ALIGN.load(Ordering::Relaxed));
    let (ox, oy, fw, fh) = rect(sw, sh, dw, dh, f, a);
    log::warn!(
        "display: fit={} bar={} align={} src={}x{} dst={}x{}@{},{} scr={}x{}",
        f.file_str(),
        b.to_hex(),
        a.file_str(),
        sw,
        sh,
        fw,
        fh,
        ox,
        oy,
        dw,
        dh,
    );
    (ox, oy, fw, fh)
}
