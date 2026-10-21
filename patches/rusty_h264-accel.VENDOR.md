# rusty_h264-accel —— submodule + patch pin（2026-10-21 起冻结，不跟上游新版）

- 上游：https://github.com/remade-with-rust/rusty_h264 @ tag v0.16.1 (1a3d065)
- 本仓形态：submodule（`third_party/rusty_h264`，frozen）+ `patches/rusty_h264-accel.patch`（功能 delta）
- 应用：`git -C third_party/rusty_h264 apply ../../patches/rusty_h264-accel.patch`
- 本文件内 'VENDOR.md' 指代即本文件；'README.upstream.md' 在 submodule 内已有原版。

---

# Vendored `rusty_h264-accel` 0.16.1 — no_std port + firmware single-bit switch

- 上游：https://github.com/remade-with-rust/rusty_h264 ，版本 **0.16.1**
  （crates.io 2026-10-19，BSD-2-Clause，`LICENSE` 见本目录；
  openh264 算法/表格归属见 `LICENSE.openh264`，随树原样保留）。
- 本目录是“上游 0.16.1 全树 + no_std 移植层”，不是 fork 上游仓库；
  `src/` 之外只留 `Cargo.toml`/`LICENSE`/`LICENSE.openh264`/
  `README.upstream.md`（上游 README 原样拷贝，头加 vendor 注）/
  `Cargo.toml.orig`（上游原始 manifest 备查）。
  上游 `examples/`（`kernel_bench` 另需 path 形态的 `rusty_h264-common`
  dev 依赖，registry 形态下编不过）与 `tests/`（`tq_scalar_arm.rs` 是
  out-of-process 门，需整 workspace）未 vendor；箱内单测跑 `src/` 里
  `#[cfg(test)]` 单元测试（`cargo test`，host + 默认特性）。
- 本仓 `decoder`（`rusty_h264-decoder`）保持 crates.io 外部箱不动
  （`default-features = false` + `libm`，`asm` 见下）；`common`
  （`rusty_h264-common` 0.16.1）同样外部箱不动；根 `Cargo.toml` 以
  `[patch.crates-io]` 把本目录截到 `common` 的 `accel` 可选依赖上
  （`version` 保持 0.16.1，特性名 `census`/`knobs` 与上游一致，
  兼容 `common` 的 `accel?/` 弱依赖）。

## 改了什么（相对上游 0.16.1）

1. 加 `std` 特性（`--features std` 即与上游行为完全一致）；`default` 为空、
   不含 `std` —— 不是疏忽，是 cargo 特性单向累加逼的：`rusty_h264-common`
   （外部箱，不可改）以 `optional = true`（未关 defaults）拉本箱，
   本箱默认特性会被固件图无条件打开；默认含 `std` 即链 `std`，
   终链（连 `cargo check --target x86_64-unknown-uefi` 都过不了，
   2026-10-21 实锤）死于 E0152（`panic_impl` 与 `uefi` 撞车）。
   关掉 `std`（默认即关）即 `#![no_std]` + `alloc`
   （`extern crate alloc` + 按文件 `use alloc::…`）。
   与 h265-accel（`default = ["std", "simd"]`，其父箱是本仓可控的 path
   依赖、边上关了 defaults）不对称，照抄会炸，此处特记。
2. `use std::arch::…` → `use core::arch::…`（同内在函数，no_std-safe；
   上游 `x86_dc`/`nnz` 两处本来就是 `core`，其余对齐）。
3. 运行时检测：no_std 下**不做第二套 CPUID/XCR0 探**（`core` 在
   no_std 下没有 `is_x86_feature_detected!`，手写 `__cpuid` 就是第二套，
   故不写）。规则（`src/lib.rs`）：
   - `have_avx2()`：std 侧原样 `std::is_x86_feature_detected!("avx2")`
     （OnceLock 缓存，host 行为零变化）；no_std 侧恒 `true`（最高档）。
   - 所有 `std::is_x86_feature_detected!("avx2")` 直调点（`luma_mc` w16
     档、`idct4x4` SSE4.1 fused、`dequant_4x4`、`satd_sad` AVX2 宏）
     与 `is_aarch64_feature_detected!("neon")`（aarch64 基线恒有 NEON）
     在 no_std 下同样恒取真。
   - 真门是固件推下来的：`resolve_accel` 按 `copy_width()` 单比特推
     （`scalar` → `set_simd_enabled(false)`，其余 → `true`，开机一次；
     `avc_probe` 首解前再幂等 `ensure_xsave_enabled` 一次，
     防 OVMF 默认 `CR4.OSXSAVE=0` 下 XGETBV 的 `#UD`）。
4. 新增固件总开关（上游无此缝，VENDOR 新增；host 默认行为不变）：
   `static SIMD_ENABLED: AtomicBool`（`core::sync::atomic`，两构建同类型；
   初值恒 `true` —— 上游本就没有关，上游一致即开；`default` 为空后 host
   默认构建无人来推，初值关会把 host 全部打成标量）+
   `pub fn set_simd_enabled(bool)` + `pub(crate) fn simd_enabled()`。
   固件 `resolve_accel` 开机无条件推一次（`scalar` 关/其余开），
   首解只能跑在它后面，故初值在固件里永不实际执行。关时每个解码可达
   入口走标量双胞胎（`hpel_fused`/`dequant_4x4` 回 `false`、
   `bs_motion_masks*`/`mb_uniform` 回 `None`，调用方（`common`，外部箱
   不动）本来就有标量回退；census 记 `scalar()`，单比特关不断 census 口径）。
5. 只给**解码可达**入口加门（`common` 调得到的：`idct4x4_*`/
   `flat_add`/`luma_dc_from_scan`/`nnz`（纯标量，本就无门）/
   `deblock_*` 8 入口（经 `dispatch!` 宏统一加门）/`mc_*`/`pixel_avg`/
   `mc_chroma_w*`/`hpel_fused`/`bs_motion_masks*`/`mb_uniform`/
   `dequant_4x4`）。编码侧（`satd_*`/`sad_*`/`mectx`/`satd_avg`/
   `dct_four_t4`/`idct_four_t4_rec`/`quant_four_4x4`）固件无调用者，
   只做编译期 no_std 化（`have` 恒真），不加运行时门、不改 host 行为。
   `intra_pred` 本就是纯标量 Rust，无门可加。
6. `knobs` 特性下 `RFF_ABL_RECON`/`RFF_TQ_SCALAR`：std 侧原样
   （OnceLock + `AtomicU8` 缓存 + `std::env`）；no_std 侧 env 读作未设置
   （恒 `false`，抄上游 `common` 的 knob 主义；`AtomicU8` 走 `core`）。
   固件不开 `knobs`（`asm` 不隐含它），此路恒关。
7. `census` 模块本来就是 `core::sync::atomic` + `&mut dyn FnMut`，
   no_std 下零改动。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点；
合上游新版时按注释逐处重放即可。

## 固件门契约（`asm` 开 + 无 `std` 的用法）

- 开机 `config::resolve_accel` 先 `ensure_xsave_enabled`（CPL0 幂等开
  XSAVE）再按 `copy_width()` 单比特推开关（`scalar` 关/其余开）；
  首解前 `avc_probe` 再幂等 `ensure` 一次。开关是唯一运行时门，
  轮内无第二套 CPUID/XCR0 探。
- `simd_enabled() == false` 时全入口标量双胞胎（census 全记 scalar），
  与 `asm` 关掉的纯标量构建逐字节一致（host 双路一致性门控）。
- 遗留风险（实锤，非推测）：开状态（初值即开；`resolve_accel` 必推）假设
  AVX2/SSE4.1 可执行（no_std 下 `have` 恒最高档，见上）：2008 年前无 SSE4.1 /
  2013 年前无 AVX2 的 CPU 上 `level != scalar` 会 `#UD`
  （`deblock` 的“sse2”核本就无条件用 SSSE3 `pabsw` + SSE4.1 `pblendvb`，
  这是上游原样假设，非本移植引入；`hpel_fused`/w16 档/`dequant_4x4` 的
  AVX2 门在 no_std 下恒开）。受支持硬件（AVX2 机、QEMU `host`/hvf）
  无此问题；QEMU `qemu64`-TCG 口径由另立项的 `avc-probe` case 定
  （`cpu` 钉法见该立项）。
- `set_simd_enabled` 必须在首解前推（`resolve_accel` 开机无条件推，
  首解只能跑在它后面；初值开 == 上游行为，忘推在固件里不可能发生）。

## 本仓约定豁免

- 这是第三方上游代码（BSD-2-Clause，版权归 Mata Network），**不受**
  “单文件 ≤1000 行”约束（`deblock_simd.rs`/`luma_mc.rs` 等本就超限）——
  只要求新增的移植层自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生大量无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
