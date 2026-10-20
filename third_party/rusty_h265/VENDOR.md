# Vendored `rusty_h265` (+ `rusty_h265-accel`) — no_std port (scalar)

- 上游：https://github.com/Remade-With-Rust/rusty_h265 ，版本 **0.6.2**
  （crates.io 2026-10-17，Apache-2.0，`LICENSE` 见本目录/隔壁同名文件）。
- 本目录是“上游 0.6.2 全树 + no_std 移植层”，不是 fork 上游仓库：
  上游目前**不支持 no_std**（`std::Arc/VecDeque/原子量/env` 无特性门，
  2026-10-21 实测 `cargo check --target x86_64-unknown-uefi` 能过只是因为
  本机新工具链自带 UEFI 的 `libstd`，链接/运行仍不可用，且本仓禁固件侧 `std`）。
  等上游自己长出 no_std 分层（它家 h264 就是这么走过来的）就切回 crates.io，
  把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 0.6.2）

1. 两箱各自加 `std` 特性（默认开，行为与上游完全一致）；
   关掉即 `#![no_std]` + `alloc`（`extern crate alloc` + 按文件 `use alloc::…`）。
2. 口径（抄上游 h264-common 的 shipped-defaults 主义）：无 std 时环境变量
   开关读作未设置、诊断打印变 no-op，解码走默认生产路径。
3. `simd` 只在 `std` 下验证过；固件用 `default-features = false`
  （= 无 std + 无 simd，纯标量）。`simd`+无 `std` 组合**未移植**。
4. `[[test]]`（conformance 需要外部向量文件，包里没有）在此去掉了；
   `readme` 指向保留的上游 README；`bench-alloc` 的 `=2.2.6` 精确 pin 是
   上游原意（测量臂的分配器同一性），可选依赖，固件侧永不启用。
5. 上游 `census`/`cabac-trace`/`prof` 等 dev 门保持原样（`prof` 本来就特性门）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点；
合上游新版时按注释逐处重放即可。`src/bin/` 随树保留（host 可用，固件作依赖时
不编译）；`tests/` 未 vendor（conformance 需要包外向量文件，跑不起来）。

## 本仓约定豁免

- 这是第三方上游代码（Apache-2.0，版权归 Mata Network），**不受**
  “单文件 ≤1000 行”约束（`decoder.rs` 等本就超限）——只要求新增的移植层
  自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
 （会产生数千行无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
