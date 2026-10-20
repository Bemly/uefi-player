# Vendored `filmcraft-mpeg2v` — no_std port (scalar)

- 上游：https://github.com/storytold/filmcraft `crates/mpeg2v`，git
  **70f0fc7a**（2026-10-20，workspace version 0.10.0，MIT OR Apache-2.0，
  `LICENSE-MIT`/`LICENSE-APACHE` 见本目录）。
- 本目录是“上游 `crates/mpeg2v` 全树 + no_std 移植层”，不是 fork 上游仓库：
  上游目前**不支持 no_std**（`std::sync/Mutex/OnceLock`、隐式 std prelude、
  `f64::cos` 无特性门），且该箱**未发布到 crates.io**（直接依赖不可能；
  本仓除自家轮子外不用 git 依赖），故走 path vendor。
  等上游自己长出 no_std 分层并发布，就切回 crates.io，把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 70f0fc7a）

1. 加 `std` 特性（默认开，行为与上游完全一致）；
   关掉即 `#![no_std]` + `alloc`（`extern crate alloc` + 按文件 `use alloc::…`，
   与 vendored rusty_h265 / 上游 h264-common 同一写法）。
2. `threads`（rayon 行并行，MPEG-1 本来就走顺序路）保持上游特性门；
   `threads`+无 `std` 加 `compile_error!` 明示不支持（rayon 要 std；
   抄 rusty_h265“`simd` 隐含 `std`、未移植组合明确不支持”主义）。
3. 本箱**无 SIMD 概念**（纯标量 `f32` IDCT，无 `arch/target_feature` 检测，
   workspace 级 `unsafe_code = "forbid"`）：固件用 `default-features = false`
   即纯标量可用，没有 rusty_h265 那种“simd 留 std-only”的纠结。
4. `OnceLock` 查表（idct 基表、vlc 两级表）：`core::cell::OnceCell` 不是
   `Sync`（`static` 拒绝，编译器实锤），故用 tiny `spin::Once` 无条件替换
   （单次初始化语义等价，std/no_std 零 cfg；MIT，MSRV 1.38，
   `default-features = false + features = ["once"]` 零传递依赖）。
   等价性由箱内表测试 + IEEE 1180 + 解码单测在双配置下共同验证。
5. `SliceErrors::first` 的 `std::sync::Mutex`：std 侧保留（rayon 多线程收错），
   no_std 侧（threads 恒关、单线程）折 `core::cell::Cell`（`into_inner→take`，
   约 5 行）。
6. 浮点方法（编译器实锤，共 4 处）：IDCT 建表两处 `.cos()` 与像素热路两处
   `.floor()` 改无条件 `libm::cos` / `libm::floorf`（std/no_std 同表同值，
   diff 最小；`libm` 已在本仓固件闭包内——经 `rusty_h264-decoder`，复用零新增
   风险；箱内 IEEE 1180 精度单测当场验证替换无损）。
7. host 测试构建显式 `#[cfg(test)] extern crate std`：上游 `#[cfg(test)]`
   模块原样不动（用 `std::` 路径），生产 no_std 代码不受影响。
8. `thiserror` 无条件 `default-features = false`：本箱只格式化 `String`
   载荷，`std` 特性（Path/PathBuf 便利）无行为差异（上游 thiserror 2.0.21
   官方归类 no-std，MSRV 1.77）。
9. 去掉 `tests/`（ffmpeg 外部 oracle + 生成 fixture，包外向量跑不起来）、
   `src/synth_tests.rs`（同上：shell 出 ffmpeg + 写文件，其 `mod` 行一并去掉，
   注释说明）、`[dev-dependencies] filmcraft-testkit`、`[lints] workspace`
   （vendor manifest 自包含；`unsafe forbid` 由源码本身保证——全树无 unsafe）。
10. `README.md` 上游原样保留。

## MSRV

- 上游 workspace 声明 `rust-version = "1.95"`，**高于本仓线 1.92**；
  本 vendor manifest 按实测 floor 钉到 `1.92`（合上游新版时重测），
  实际用到的语言特性全部 ≤1.88（let-chains 1.88、`is_none_or/is_some_and`
  1.82、`repeat_n` 1.82、`div_ceil` 1.73、let-else 1.65），依赖 floor：
  thiserror 1.77 / libm（0.2 线，实测过）/ spin 1.38。
  vendor 实测结论：**1.92 工具链 `cargo check`（默认/no-default 双形，
  零警告）+ `cargo test --no-default-features`（15+1 通过）全绿**
  （2026-10-21 实测）。

## 精度口径（设计时定死，后人勿改）

MPEG-2 的 IDCT 按定义不是 bit-exact（任何 IEEE 1180 准确的 IDCT 都合规，
上游 README 原话）：**逐帧 max|Δ| ≤ 4、PSNR ≥ 58dB、差异样本 ≤ 3%**；
帧数、显示序、图像类型、交织/场序精确。固件侧 probe 校验和**只取 Y 平面**
（handover §7.3 血泪：host 拼帧输出含色度，固件侧只加 Y，口径不对先对口径）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点（crate 属性、
各文件 import 前奏、SliceErrors 三处、`spin::Once` 两处、cos/floorf 四处、
`compile_error!`、synth_tests 去掉、test 用 `extern crate std`）；纯机械的
`std::→core::` 单 token 替换（`Range/mem::take/Result/from_fn/repeat_n`）
无逐行标记，合上游新版时 `grep -rn "std::" src/` 列全量按本文件分类表重放即可。

## 验证记录（2026-10-21）

- host 4 配置 check 全绿：默认 / `--no-default-features` /
  `--no-default-features --features std` / `--target x86_64-unknown-uefi`
  （以固件 target 为准；`--no-default-features --features threads`
  预期 `compile_error!`，属设计行为）。
- 箱内单测通过（vlc Kraft 律/DCT 表、IEEE 1180 精度、headers/mc/bits、
  hand-built 整帧 + 截断/变异不 panic；synth/oracle 需 ffmpeg 未带）。
- 固件 `cargo build --target x86_64-unknown-uefi` 链接通过
  （path 依赖随主包进终链；`cargo check` 能过≠能用，见 handover §7.3）。

## 本仓约定豁免

- 这是第三方上游代码（MIT OR Apache-2.0，版权归 ArtCraft Team），**不受**
  “单文件 ≤1000 行”约束——当前最大 `lib.rs` 约 830 行，原生即合规；
  新增的移植层自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
