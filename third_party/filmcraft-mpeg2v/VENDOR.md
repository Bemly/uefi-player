# Vendored `filmcraft-mpeg2v` — no_std port (scalar + `simd`)

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
- 2026-10-21 用户再拍板：**必须上 SIMD**（推翻同日稍早“标量已够”结论，
  见下“SIMD（一期）：自研整数 SSE2”）：`simd` 特性（`std` 蕴含），定点
  IDCT + `pavgb` 预测向量化，标量双胞胎恒编译，双路互锁。

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
   （vendor manifest 自包含；`unsafe forbid` 由源码本身保证——SIMD 前全树无
   unsafe；SIMD 后仅 `idct.rs`/`mc.rs` 内含 `asm!` 的 `unsafe fn`，见下）。
10. `README.md` 上游原样保留。
11. 新增 `simd` 特性（`std = ["simd"]` 蕴含；`default-features = false` +
    `simd` 即 no_std 固件 SIMD 形）：零新依赖（纯 `core::arch::asm!` +
    `core::sync::atomic`），`#![no_std]`-clean，MSRV 安全（`asm!`
    1.59 起稳定）。`set_simd_enabled(bool)` 公开（无 `simd` 时 no-op，
    调用方无需 `cfg`），默认 `std` 下 ON、无 std 下 fail-closed OFF，
    与 h263-rs 的 `yuv`/`deblock` 开关同形（见该目录 VENDOR.md）。
12. IDCT 定点化重写（替代 `f32` 路，见 `src/idct.rs` 文件头）：`S = 16`
    定点基表（`round(M·2^16)`，`|C'| ≤ 32138 < 32767`，`basis_fits_i16`
    单测锁死）+ 直接点积（exact `i64` 累加，单次最终舍入）+ DC 项精确折叠
    （与 `dc_only` 快路逐值一致，`dc_only_matches_the_full_transform`
    原样通过）。行核双路：标量 twin（恒编译）+ `pmaddwd` 整数 SSE2
    `asm!` 核（`#[target_feature(enable = "sse2")]` 双保险，`movdqu` 全程，
    无对齐要求、无 `nt`/`sfence`）；列核 + 最终舍入是双路共享的标量尾，
    故双路逐比特一致是构造性的（32 位模点积喂精确 64 位累加——求和顺序
    无关）。输入先饱和到 ±32768（`i16` 道宽；合规流与全部精度/oracle
    输入都是 no-op，只钳住对抗输入，保证永不溢出/panic，见文件头界）。
13. `predict` 半像素平均向量化（`pavgb` 形，整数，见 `src/mc.rs`）：
    两抽头 `(a+b+1)>>1` = `pavgb`（精确），四抽头
    `(a+b+c+d+2)>>2` 经 `punpcklbw→paddw→psrlw→packuswb`（和 ≤ 1022，
    16 位精确；`packuswb` 值恒 ≤ 255，无饱和），`store` 平均侧同 `pavgb`
    原地形。宽度特化直道（8/16，`cmb()` 证明的仅有取值）+ 标量 fallback；
    开关每 `predict` 调用查一次（循环不变量，完美预测），行内无原子。
    边缘钳制慢路双路共享标量。SSE2-only：无 AVX、无 XSAVE 牵连。
    血泪：`lateout` 临时量可与 `in(xmm_reg)` 输入复用同一寄存器
    （`lateout` 语义=输入先读），故模板内晚读的向量输入会被早先的
    `movq` 覆盖——`+2` splat 改在模板内现造（`pcmpeqd→psrlw→paddw`），
    GPR 指针输入免疫（寄存器类不同）。双路差 1 抓包实锤过这一条。

## MSRV

- 上游 workspace 声明 `rust-version = "1.95"`，**高于本仓线 1.92**；
  本 vendor manifest 按实测 floor 钉到 `1.92`（合上游新版时重测），
  实际用到的语言特性全部 ≤1.88（let-chains 1.88、`is_none_or/is_some_and`
  1.82、`repeat_n` 1.82、`div_ceil` 1.73、let-else 1.65；新增的 `asm!`
  1.59 起稳定、`lateout _` 占位同代），依赖 floor：
  thiserror 1.77 / libm（0.2 线，实测过）/ spin 1.38（新增 `simd` 零新依赖）。
  vendor 实测结论：**1.92 工具链 `cargo check`（默认/无默认/无默认+`simd`
  三形，零警告）+ `cargo test --no-default-features --features simd`
  （20 通过）全绿**（2026-10-21 实测）。

## 精度口径（设计时定死，后人勿改）

MPEG-2 的 IDCT 按定义不是 bit-exact（任何 IEEE 1180 准确的 IDCT 都合规，
上游 README 原话）：**逐帧 max|Δ| ≤ 4、PSNR ≥ 58dB、差异样本 ≤ 3%**；
帧数、显示序、图像类型、交织/场序精确。固件侧 probe 校验和**只取 Y 平面**
（handover §7.3 血泪：host 拼帧输出含色度，固件侧只加 Y，口径不对先对口径）。

## SIMD 评估（2026-10-21）：先判“标量已够”→ 用户拍板必须上 SIMD，自研整数 SSE2

- 热路：`src/idct.rs`（旧 `f32` 可分 2-D IDCT：行 8×(4+4) 乘 + 列 8 列×(8+8) 乘 +
  `floorf` 落点，`dc_only` 快路）→ `src/slice.rs::block/macroblock`（每 MB 至多
  6 块）→ `src/mc.rs::predict`（半像素平均，最大 16×16，即 `pavgb` 形状的整数
  平均）；`vlc/bits/headers` 是串行熵解码，不可向量化。
- 轮子（2026-10-21 实搜 crates.io API + 逐个下载验源码，结论：都不用）：
  `wide 1.7.1`（累计 ~84.0M 下载，Zlib OR Apache-2.0 OR MIT，MSRV 1.89，
  本身 `#![no_std]`）——不用：通用 SIMD 底座、不是 MPEG-2 定点 IDCT/
  `pavgb` 核；内在函数实现违反本仓 `asm!` 优先纪律，自带分发等于第二套
  CPUID（禁）。
  `safe_arch 1.2.0`（累计 ~82.7M 下载，同 license，MSRV 1.89，`no-std`
  分类）——不用：只是 `core::arch` 内在函数薄包装（同违反两条）。
  `pulp 0.22.3`（累计 ~31.9M 下载，MIT，MSRV 未声明，
  `default=["std","x86-v3",…]`）——不用：默认即假设 AVX2 基线（与 UEFI
  SSE2 基线 + XSAVE 门控直接冲突），自带运行时分发（禁）。
  `oxideav-mpeg12video 0.0.13`（~2.5k 下载，MIT，MSRV 1.80，唯一“确切需求”
  的纯 Rust MPEG-1/2 解码轮）——不用：`f64` 字面双重求和标量 IDCT（比现有
  `f32` 更慢）、零 SIMD、无 no_std（`std::` 散布各模块），且整解码器替换
  超出热路优化范围（现有 vendor 树已验证 68dB + IEEE 1180）。
  `signal-transforms 0.1.5`（~5.2k，MIT，MSRV 未声明）——不用：通用数学库，
  依赖 `nalgebra`（重型 std 矩阵）+ `std::f32`，无 MPEG-2 定点核。
  `libjpeg-turbo-rs 0.8.0`（~70k，MIT OR Apache-2.0，MSRV 1.87，
  自称 no_std-capable）——不用：JPEG 融合核（dequant+IDCT+level-shift 一体，
  与 MPEG-2“分离 dequant + /8 IDCT + 回加预测”流程错层）+ 自带 `cpu_has!`
  分发（第二套 CPUID，禁）+ 为一核拖整个编解码器（错层依赖）。
- 自研（用户拍板）：① IDCT 定点化重写替代 `f32` 路（`S = 16` direct-dot，
  上述“改了什么”§11–12，IEEE 1180 精度门全过）；② `predict` 半像素平均
  `pavgb` 向量化（§13，整数）；③ 纪律：`asm!` 显式向量寄存器优先（防
  LLVM 标量化），浮点向量不执行，SSE2-only（无 AVX/AVX2，故无
  CR4.OSXSAVE/XCR0 门需求），`nt`+`sfence`+对齐门控 N/A（小复用行缓冲，
  `movdqu`+时间局部性，注释写明），统一走 `config::copy_width()` 单比特
  （`scalar`=关/else=开），箱内无第二套 CPUID/XCR0 探。
- 精度证据（ffmpeg 9.0.1 双解，Y 口径，I-only/I+P+B/纯黑/灰/白 + 新增
  SD704×480 intra-hard 与 I/P/B 混合共 7 流，帧数全部精确一致）：

  | 流 | max\|Δ\| | PSNR | 差异样本 | 新旧（定点 vs 旧 f32） |
  |---|---|---|---|---|
  | 64×64 I-only ×10 | 1 | 66.13dB | 1.587% | 逐字节一致 |
  | 64×64 I/P/B ×10 | 1 | 67.86dB | 1.064% | ±1 内（旧 67.90dB） |
  | 64×64 纯黑/灰/白 ×5 | 0 | inf | 0 | 逐字节一致 |
  | SD704×480 intra-hard ×60 | 2 | 72.59dB | 0.355% | 门内 |
  | SD704×480 I/P/B ×60 | 2 | 73.68dB | 0.277% | 门内 |

  用户门（max\|Δ\|≤4 / PSNR≥58dB / 差异≤3%）全过；双路（`set_simd_enabled`
  ON/OFF）7 流逐字节一致。口径教训保留：ffmpeg `gray` 引用会做
  limited→full 拉伸，必须用 yuv420p 首平面取 Y（已钉死在本节）。
- 性能（host release，仓外 Rust harness 直测本箱真实代码，预载码流去
  IO 方差，进程内逐轮交替，best-of；宿主 i5-10400 当时 load ~14，数字取
  best，p50 同趋势）：新双路任一 vs 旧 `f32`：SD 混合 ~1.3×
  （best 1.1ms→0.86ms/帧）；SIMD vs 标量双胞胎：parity（±3%，intra/SD
  混合多轮 best/p50 一致）。IDCT 微基准：SIMD 行核 ~413ns/块 vs 标量
  ~490ns（1.2×）。解读：定点重写是主要胜利（去 `libm::floorf`×64 +
  `f32↔int` 转换）；`pavgb`/行核的向量红利被 VLC/反量化等标量份额
  （Amdahl）稀释；SIMD 路的价值是跨 flags/工具链的保证向量
  （AGENTS 纪律：LLVM autovec 在此机吃到红利，不保证处处吃到），
  不是在此机上拉开差距。固件侧不同 flags 下 autovec 可能更弱，
  asm! 相对更优——预期，不 claim。
- `threads` 恒关、`spin` 现状不变、`libm` 保留（建表 `cos`+舍入 `round`
  各一处；热路零浮点）、`framework/encoder` 不适用本箱。
- 固件契约（`[accel]` 接线座，`src/` 侧由固件 owner 接——本箱只暴露
  `set_simd_enabled`，永不自探）：`resolve_accel` 按 `copy_width()`
  推单比特（`scalar`=关/else=开，现有 hevc/vp9/h263 四家同款再加一行
  `filmcraft_mpeg2v::set_simd_enabled(on)` + 可选 `describe` 行供矩阵
  断言）；固件要吃到 SIMD 另需根 `Cargo.toml` 给本箱加 `features =
  ["simd"]`（当前 `default-features = false` 无 `simd` = 驻标量双胞胎，
  安全默认）。`simd` 无 `std` 组合已在箱内全测（`threads`+无 `std`
  仍 `compile_error!`，属设计行为）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点（crate 属性、
各文件 import 前奏、SliceErrors 三处、`spin::Once` 两处、cos/round 建表 +
`set_simd_enabled` 开关、行列双路分发、`compile_error!`、synth_tests 去掉、
test 用 `extern crate std`/`alloc` 前奏）；纯机械的
`std::→core::` 单 token 替换（`Range/mem::take/Result/from_fn/repeat_n`）
无逐行标记，合上游新版时 `grep -rn "std::" src/` 列全量按本文件分类表重放即可。
SIMD 核（`idct.rs::row_pass_simd` + `mc.rs::avg2/avg4/store_avg_*` +
`set_simd_enabled` 开关 + 双路互锁单测）是本仓新增，无上游对应物。

## 验证记录（2026-10-21，SIMD 一期）

- host 4 配置 check 全绿：默认 / `--no-default-features` /
  `--no-default-features --features simd` / `--target
  x86_64-unknown-uefi`（`--no-default-features --features simd` 固件形，
  以链接为准；另验 `--target` 无默认形亦链接；
  `--no-default-features --features threads` 预期 `compile_error!`，
  属设计行为）。
- 箱内单测：默认 20 通过 / 无默认 16 通过（`simd` 门控 4 项只在前者跑；
  另 `basis_fits_i16` 双形跑）+ 新增双路一致 4 项（IDCT 宽 sweep +
  精确期望、predict 宽 sweep + 精确期望）全绿；vlc Kraft 律/DCT 表、
  IEEE 1180 精度、headers/mc/bits、hand-built 整帧 + 截断/变异不 panic
  原样通过（IEEE 1180 与 `dc_only_matches` 在新定点算法上重验通过，
  非旧 `f32` 数字）。
- host 像素回归（ffmpeg 9.0.1 双解，Y 口径，7 流）：帧数精确一致，
  用户门（max|Δ|≤4 / PSNR≥58dB / 差异≤3%）全过，双路逐字节一致，
  详见上节表格（仓外 Rust harness，`--bench` 复用同一解码路径）。
- clippy 三形（默认/无默认+`simd`/UEFI 无默认+`simd`）零警告；
  `cargo fmt -p uefi-player --check` 通过（禁 fmt vendor 树）；
  1.92 工具链三形 check + 无默认+`simd` 单测 20 通过。
- 固件 `cargo build --target x86_64-unknown-uefi`：本箱两形（无默认士
  `simd`）rlib 链接通过（`cargo check` 能过≠能用，见 handover §7.3）。
  根 bin 终链当时红（`src/config/mod.rs` 引 `rusty_h264_accel` 无直接
  依赖 + 其 std 进终链 E0152，系同树并行改动，非本箱问题；本箱无 std
  依赖，见 Cargo.toml）。

## 本仓约定豁免

- 这是第三方上游代码（MIT OR Apache-2.0，版权归 ArtCraft Team），**不受**
  “单文件 ≤1000 行”约束——当前最大 `lib.rs` 约 830 行，原生即合规；
  新增的移植层自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。
