# Vendored `rusty_h265` (+ `rusty_h265-accel`) — no_std port (scalar + simd)

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
3. `simd` 已 no_std 化（2026-10-21，本轮）：`simd` 不再隐含 accel `std`；
   不开 `std` 时环境旋钮读作未设置（整机默认值，沿 handover §7.2 knob 主义）、
   `std::arch`/`std::slice`/原子量走 `core` 同类型路径、运行时 ISA 不再自带
   第二套 CPUID/XCR0 探（no_std 下 `have` 恒取最高档，固件推下来的 `FORCED`
   才是真门；`simd`+无 `std` 组合至此可用）。
4. `[[test]]`（conformance 需要外部向量文件，包里没有）在此去掉了；
   `readme` 指向保留的上游 README；`bench-alloc` 的 `=2.2.6` 精确 pin 是
   上游原意（测量臂的分配器同一性），可选依赖，固件侧永不启用。
5. 上游 `census`/`cabac-trace`/`prof` 等 dev 门保持原样（`prof` 本来就特性门）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点；
合上游新版时按注释逐处重放即可。`src/bin/` 随树保留（host 可用，固件作依赖时
不编译）；`tests/` 未 vendor（conformance 需要包外向量文件，跑不起来）。

## 固件门契约（`simd`+无 `std` 的用法，2026-10-21）

- 检测集只有一套，在固件侧：开机 `config::resolve_accel` 先
  `ensure_xsave_enabled`（CPL0 幂等开 XSAVE）再 `probe_max_width`
  （CPUID/XCR0，与 `check_avx_support`/`hw_bench` 同位），按 `copy_width()`
  （= min(配置, 硬件)）推 force 上限：U64→`force_scalar`（真标量驻留；
  `force_isa(Scalar)` 上游语义是 SSE2 档位故单列新 API）、X128→Baseline、
  V128→Sse41（轮侧无 VEX-128 档；AVX 机器恒有 SSE4.1，可安全执行）、
  Y256/Z512→Avx2。首解前 `hevc_probe` 再幂等 `ensure` 一次。
  no_std 下首次 `probe()` 若没收到 push，debug 直接断言失败（防无门禁跑 AVX2）。
- 纪律审计（本轮实锤）：全箱整数核——无 f32/f64 向量、无 `_ps/_pd/cvt`
 （唯一 `f64` 在 `#[cfg(test)]` 计时打印里）；无 nt store（零
  `movnt/stream`，故无 `sfence`/对齐债）；AVX2 双胞胎全在
  `#[target_feature(enable = "avx2")]` 内、只经 `isa() == Avx2` 可达；
  SSE2/SSE4.1 双胞胎 `#[target_feature]` 声明与调用点一致（SSE2 是 x86-64
  长模式基线，EDK2 自身亦依赖）。
  双胞胎保持上游内在函数形态、不重写 `asm!`：① 本仓 flags
  （`.cargo/config` 的 `-soft-float,+sse,+sse2`）下 codegen 正确，
  终链通过即准绳（E0152/LLVM ICE 两道旧门皆过）；② 重写毁合流能力；
  ③ 软浮点标量化只伤害浮点向量，本箱无浮点向量。

## 本仓约定豁免

- 这是第三方上游代码（Apache-2.0，版权归 Mata Network），**不受**
  “单文件 ≤1000 行”约束（`decoder.rs` 等本就超限）——只要求新增的移植层
  自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
 （会产生数千行无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
