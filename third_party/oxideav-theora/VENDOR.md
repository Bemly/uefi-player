# Vendored `oxideav-theora` — no_std port (decoder-only scalar)

- 上游：https://github.com/OxideAV/oxideav-theora ，版本 **0.0.12**
  （crates.io 2026-09-01，MIT，`LICENSE` 见本目录；`README.upstream.md`
  为上游 README 原样保留）。
- 本目录是“上游 0.0.12 全树（去 tests/examples/fuzz，见下）+ no_std 移植层”，
  不是 fork 上游仓库：上游目前**不支持 no_std**（`std::collections`/`std::fmt`/
  `std::error` 无特性门；`cargo check --target x86_64-unknown-uefi` 能过只是因为
  本机新工具链自带 UEFI 的 `libstd`，链接/运行仍不可用，且本仓禁固件侧 `std`）。
  等上游自己长出 no_std 分层就切回 crates.io，把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 0.0.12）

1. 三箱特性 `default = ["std", "framework", "encoder"]`（默认开，行为与上游完全一致）；
   关掉即 firmware 形：`#![no_std]` + `alloc`（`extern crate alloc` + 无条件 alloc
   前奏：与 std 前奏同类型，`std` 构建不受影响）+ 无 framework + 无 encoder。
2. 口径：`std` 关时无 env/eprintln（本箱生产代码本来就零 env、零 eprintln，
   只有 tests 用）；诊断 Display 沿用上游的 `core::fmt` 实现。
3. `framework`（默认开）是 oxideav-core 胶水（TheoraDecoder/TheoraEncoder/
   register/make_*/cropped_to_video_frame/HeaderState/plane_to_top_down +
   `use oxideav_core::RuntimeContext`），以及 `oxideav-core` 可选依赖。
   `framework` 隐含 `std`：上游 oxideav-core 是 std-only
  （thiserror/serde_json/`std::io`，无特性可关），bad 组合用特性蕴含直接堵死。
   这是比 rusty_h265 稍大的上游 delta：h265 保留了兄弟箱依赖，本箱把 20k 行的
   框架箱依赖改成了可选——固件侧永远关掉，只留 `FrameDecoder` 裸解码路径。
4. `encoder`（默认开）是 FrameEncoder + 全部前向/RD/码率控制机器
  （含 `vp31_huffman_data` 整模块、`BitWriter`、Token 规划器、Rate* 族、
   `SourceFrame`、`with_tuned/with_gop_tuned` 等方法级门），同样隐含 `std`：
   唯一的 std-only 调用 `RateModel::x` 的 f64 `.ln()` 在 no_std 下无对应方法，
   首轮不碰（libm 跟进是另立项；h264 有 libm 先例）。
   固件 `default-features = false` = 纯标量解码器，无 SIMD 概念可留
   （本箱零 intrinsic/`target_feature`，关掉特性即全部）。
5. `register_codecs` 的 `.encoder(make_encoder)` 一行拆成 encoder 门控
   （decoder 注册恒在；见该处 `VENDOR(uefi-player):` 注释）。
6. `[[test]]`/examples/fuzz 未 vendor：examples（rd_ladder 丈量台）、fuzz 子箱是
   host-only 台架；tests 目录未 vendor（嵌入式单测在 `src/lib.rs` 的 `mod tests`
   里，随树保留，host 可跑；语料库不在 git 树，无外部向量可带）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点（本轮约 113 处）；
合上游新版时按注释逐处重放。`src/bin` 本箱没有（上游台架在 examples/，未 vendor）；
`tests/` 未 vendor（见上）。

## 本仓约定豁免

- 这是第三方上游代码（MIT，Mark Karpeles），**不受**“单文件 ≤1000 行”约束
  （`src/lib.rs` 单文件 44k 行，上游即如此）——只要求新增的移植层自身保持
  小而集中（本轮实际只有 crate 根前奏 + 逐处 2 行门）。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生巨大无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`
  （`--all` 会连带 path 依赖一起查；见根 AGENTS.md「双构建」注释）。

## SIMD 评估（2026-10-21）：标量已够，不做向量化

- 热路（`src/lib.rs` 全整数；`f64` 仅 `granule_position_seconds` 一处除法，
  无向量化对象；encoder 区 44k 行单文件中的前向/RD/码率机器属 `encoder`
  门之后，decoder-only 构建已排除）：`inverse_dct_1d/2d`（§7.9.3.1 55 步
  定点 butterfly，`wrapping` + `>>16` 语义逐字对照规范）→
  `dequantize_block`（64 乘，zig-zag 查表串行）→ `reconstruct_block`
  （PRED+RES 逐点加钳，8×8）→ `lflim` + `horizontal/vertical_loop_filter_edge`
 （4-tap 响应 + 分段钳制，逐边 8 次分支；分支密集即 SIMD-hostile）；
  熵解码/运动矢量分裂/帧内预测是串行分支，Amdahl 上不可向量化。
- 轮子（与 mpeg2v 同一批实搜，结论相同：都不用）：
  `wide 1.7.1`（83.8M 下载，Zlib OR Apache-2.0 OR MIT，MSRV 1.89，
  `default-features=false` 可 no_std）——不用（h263“永不进固件”前例 +
  内在函数 + 自带分发）。
  `safe_arch 1.2.0`（82.6M 下载，同 license，MSRV 1.89，no-std 原生）——
  不用（`core::arch` 薄包装 = 内在函数，违反 `asm!` 优先；无 XSAVE 集成）。
  `pulp 0.22.3`（31.8M 下载，MIT，默认 `x86-v3` 基线）——不用
  （AVX2 基线假设撞 UEFI SSE2 基线 + XSAVE 门控；自带分发禁）。
- 自研同样不做：实测标量代价（host release，仓外 Rust harness 直测本箱
  公开整数核，视频典型稀疏输入）：`inverse_dct_1d` 10ns/次，
  `inverse_dct_2d` 90ns/块，`dequantize_block` 38ns/块，`lflim` 2ns/次；
  外推 1080p 4:2:0 **全块 coded 最坏**：IDCT 4.4ms + 反量化 1.8ms +
  环路滤波 2.6ms ≈ 8.8ms/帧（典型 30~50% coded + 跳边 ≈ 2~4ms），
  相对 33ms 预算 headroom 充足；手写 55 步 butterfly 的 `asm!` 版本省
  最多 ~2ms（6% 预算），但要逐位复刻 wrapping/截断语义，风险收益比为负。
  `copy_block` 矩阵搬 BGRA 帧缓冲，与 i16 系数/u8 参考平面通路无关，无复用点。
- 正确性证据：箱内嵌入式单测（规范逐字 IDCT 向量、zig-zag、dequant、
  环路滤波足迹）双配置通过；整帧 ffmpeg 双解比对本次**未做**——被 OggS
  拆帧卡住（本箱吃 packet，固件侧 Ogg 拆帧另立项，见下；无拆帧即无整帧
  输入），列为遗留风险，待拆帧落地后按 mpeg2v 口径（Y 平面，max|Δ|≤4 /
  PSNR≥58dB / 差异≤3% / 帧数精确）补做。
- `framework`/`encoder` 门保持（decoder-only 口径不变），零 `src/` 改动，
  故无新增 `VENDOR(uefi-player):` 注释、无 fmt 影响。
  重启条件：Ogg 集成后实机 profile 显示本箱 >30% 帧预算，或上游自带
  no_std SIMD twins（届时走 h263/vp9 的 std 门控主义）。

## Ogg  carriage（固件侧另立项，不在本箱）

本箱只吃已拆帧 packet（§6.1 分类 + §A.2.3 granule 映射是公共 API，
decoder-only 构建保留），OggS 页拆帧不在箱内。固件侧手写 OggS 拆帧
（100% Rust）另立项；校验和口径先定死只取 Y 平面（Theora 输出三平面，
下原点行序，`crop_for_display` 后翻转 + YUV→BGRA 是固件侧集成活）。
