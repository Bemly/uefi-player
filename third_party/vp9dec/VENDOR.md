# Vendored `vp9dec` — no_std port (scalar)

- 上游：https://github.com/ChanyaVRC/vp9dec ，版本 **0.1.1**
  （crates.io 2026-07-26，MIT，`LICENSE` 见本目录）。
- 本目录是“上游 0.1.1 全树 + no_std 移植层”，不是 fork 上游仓库：
  上游目前**不支持 no_std**（`std::sync::Arc/原子量/env` 无特性门，
  `std::thread::scope` 并行、AVX2 运行时检测无标量开关；
  `cargo check --target x86_64-unknown-uefi` 能过只是因为
  本机新工具链自带 UEFI 的 `libstd`，链接/运行仍不可用，且本仓禁固件侧 `std`）。
  等上游自己长出 no_std 分层就切回 crates.io，把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 0.1.1）

1. 加 `std` 特性（默认开，行为与上游完全一致）；
   关掉即 `#![no_std]` + `alloc`（`extern crate alloc` + 按文件 `use alloc::…`）。
2. 口径：无 std 时线程并行恒走顺序路、AVX2 恒走标量（上游官方 315/315
   门控本来就双跑这两条路，bit-exact，丢速度不丢正确性）。
3. `simd` 留 std-only：内核模块 + 重导出整体
   `#[cfg(feature = "std")]`，分发点（predict/loop_filter/residual）
   沿上游自己的 `not(target_arch)` 惯用形状补 `not(all(std, x86_64))` 分支；
   no_std 侧 `avx2_enabled()` 恒 false。`simd`+无 `std` 组合**未移植**。
4. 线程两 driver（tile `decode_tiles_parallel`、loop_filter
   `wavefront_filter_planes`）`#[cfg(feature = "std")]`；
   no_std 侧 tile 取 `available_tile_workers() == 1`、
   loop_filter 取 `n_threads == 1`，双双漏进本就存在的顺序分支。
   仅并行用的 helper（column-strip 工人、`PlaneView`）同门控。
5. 上游 `[[test]]`（conformance/sweep/cross-check，需要包外向量文件或
   ffmpeg）在此去掉了；`tests/unit/` 随树保留（箱内单测用，缺向量干净跳过）；
   `[[example]]` 随 examples/ 一起去掉；`readme` 指向保留的上游 README。
6. 上游无 `rust-version` 声明；本仓按仓线记 `rust-version = "1.92"`
   （实际用到的最高特性约 OnceLock 1.70）。
7. `bench-timing`/`test-support` 门保持上游原样（bench 计量本就特性门零成本；
   单测要用的 `test-support` 靠自引用 dev-dep 隐式开，不污染固件构建）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点；
合上游新版时按注释逐处重放即可。`tests/unit/` 随树保留（`src/` 内 `#[path = "../tests/unit/…"]` 单测引用它，
删了箱内单测编不过）；conformance 需要的包外向量本来就不在树里。

## 验收口径（后继 vp9-probe 用）

- 像素校验和**只加 Y 平面**（抄 h265 `hevc_probe` 教训：host 拼帧输出别把色度
  加进去，固件侧只加 Y，设计时先对口径）。
- 8-bit 先行（`PlaneData::U8`）；10/12-bit `U16` 路径固件用不上，先不验。

## 本仓约定豁免

- 这是第三方上游代码（MIT，版权归 ChanyaVRC），**不受**
  “单文件 ≤1000 行”约束（`loop_filter.rs` 等本就超限）——只要求新增的移植层
  自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
