# SIMD 矩阵（`accel.level` 真分发；解码六家单比特跟随全局）

> P2 落点：`config::copy_width()`。bench 只出数（`HWBENCH`），矩阵做选择；
> `auto` = CPUID/XCR0 探最高可用（`resolve_accel`，开机一次）。

## 档位 → 位宽

| `accel.level` | 搬运位宽 | 实现 |
|---|---|---|
| `scalar` | u64 步进 | `copy_block_u64`（纯标量，最兼容） |
| `sse`/`sse2`/`sse3`/`ssse3`/`sse41`/`sse42` | 128 位 | `copy_block_x128`（`movdqu`，同宽） |
| `popcnt`/`aes`/`pclmulqdq`/`lzcnt`/`movbe`/`rdrand`/`rdseed`/`sha`/`gfni` | 128 位 | 同上（标量/密码类不加宽搬运，按基线下限同宽处理） |
| `tbm`/`sse4a` | 128 位 | 同上（AMD 专有：扩展叶 8000_0001H ECX bit 21/6；qemu64 无此叶，显式配了即按 X128 钳住用，不断 boot） |
| `avx`/`f16c` | 128 位 VEX | `copy_block_v128`（`vmovdqu` + `vzeroupper`） |
| `fma`/`avx2`/`bmi1`/`bmi2`/`avxvnni` | 256 位 | `copy_block_y256`（`vmovdqu` + `vzeroupper`） |
| `avx512f`/`avx512cd`/`avx512dq`/`avx512bw`/`avx512vl` | 512 位 | `copy_block_z512`（`vmovdqu8` + `vzeroupper`） |

- 全集 31 档（含 `auto`，按 CPUID 叶序排，`target_feature` 小写无点形命名，
  老 `"avx512"` 按 `avx512f` 照读）；`tbm`/`sse4a` 为 AMD 专有指令
  （扩展叶 8000_0001H ECX bit 21/6），已收进滑杆（末两位）：
  恒按 X128 钳住用（QEMU `qemu64` 无此叶，实测即钳住，不断 boot，矩阵不断）。
- 标量/密码类（`popcnt`/`aes`/`pclmulqdq`/`sha`/`gfni`/`lzcnt`/`movbe`/
  `rdrand`/`rdseed`）与 BMI（`bmi1`/`bmi2`）本身不加宽向量搬运：
  前者按下限 X128 处理，后者按时代下限 Y256 处理（缺 AVX2 即被钳住）。
  显式档位超硬件：警告 + 钳到硬件最大，不断 boot（`resolve_accel`）。
- `auto` 取硬件最大；`effective_accel` = `min(配置, 硬件)` 语义经 `copy_width` 落地。

## 选路位置

- `Screen::upload_direct` / `upload_frame_region`：按 `copy_width()` 分发
  （stride 感知逐行；连续整帧一次搬）。
- `video::mp_draw`：`PlayTask.copy` 快照位宽，BSP 镂空块 + AP 整块同走 `copy_block`。
- `blt_loop` / `direct_loop`：经 `upload_frame` 自动分发。
- `hw_bench` / `gop_simd`：保持全矩阵实测（bench 用，不参与选择）。

## 解码 SIMD 接线（六家单比特跟随全局，无独立开关）

> 单比特语义：`copy_width() == U64`（即 `level=scalar`）则六家全关，其余全开。
> 串口 `accel: decode simd hevc=… vp9=… h263=… h264=… mpeg2v=… theora=…`
> 值全跟随全局，另有 `accel: h264 decode isa = …` 行（hevc 同款 `isa` 行），
> 供 QEMU 矩阵断言（`hevc-probe` 的 boot_expect）。
> ee2ccad 的 `[accel.decode]` 三键新区已回滚（GUI/固件/schema/矩阵同步摘除；
> 窗口期写出的旧表直接忽略，不断解析）。

| 家 | 开关接收端 | 开时上限 | 关时 | 硬件门 |
|---|---|---|---|---|
| `hevc` | `rusty_h265::accel::{force_scalar,force_isa}` | `Baseline`/`Sse41`/`Avx2`（按 `copy_width()` 推；`V128` 取下最高档 `Sse41`） | 标量双胞胎 | 固件门（轮内无第二套 CPUID/XCR0 探） |
| `vp9` | `vp9dec::simd::set_simd_allowed` | 轮子 AVX2 max（`avx2_enabled` 再验硬件） | 顺序标量 | 轮内 CPUID/XCR0 镜像门（含首探幂等自开 XSAVE；std 另有 env knob） |
| `h263` | `h263_rs_yuv::bt601` + `h263_rs_deblock::deblock` 的 `set_simd_enabled`（两开关同推） | wide SSE2 基线核 | 标量 twin（`simd` 特性关时调用即 no-op） | 无（SSE2 是 x86-64 psABI 基线，无 XCR0 依赖；双路逐字节一致已锁死） |
| `h264` | vendor 箱自带缝 `rusty_h264_accel::set_simd_enabled`（`[patch.crates-io]` 截本地 `third_party/rusty_h264-accel`） | 轮子 max（SSE2 基线核 + AVX2；no_std 下 have 恒最高档，真门是固件推） | 标量双胞胎（`hpel_fused`/`dequant_4x4` 回 `false`、`bs_motion_masks*`/`mb_uniform` 回 `None`，`common` 自带标量回退） | 固件门（轮内无第二套 CPUID/XCR0 探；串口 `h264 decode isa` 行供断言；遗留风险见 vendor `VENDOR.md`） |
| `mpeg2v` | `filmcraft_mpeg2v::set_simd_enabled`（vendor `third_party/filmcraft-mpeg2v`，`simd` 特性下整数 SSE2 核） | 定点 IDCT 行核 + `pavgb` 预测平均 | 标量 twin（`simd` 特性关时调用即 no-op） | 固件门（轮内无第二套 CPUID/XCR0 探；SSE2-only，无 XSAVE 牵连，见 vendor `VENDOR.md`） |
| `theora` | `oxideav_theora::simd::set_simd_enabled`（vendor `third_party/oxideav-theora`，`simd` 特性下 wide 整数核） | IDCT butterfly/重建加钳/环路滤波去分支（SSE2 基线） | 标量 twin（`simd` 特性关时调用即 no-op；Ogg 拆帧另立项，本箱只吃已拆帧 packet） | 固件门（轮内无第二套 CPUID/XCR0 探；无 AVX/AVX2，见 vendor `VENDOR.md`） |

- 首解前幂等 `ensure_xsave_enabled` 保持（`resolve_accel` 开机 + `hevc_probe`
  + `avc_probe` 首解前各保一次；vp9 轮内首探自保第三道，h263 不需要）。
- 悬空历史：此前 vp9 的 `set_simd_allowed` 与 h263 两 `set_simd_enabled`
  在固件侧零调用——vp9 缺省开（轮内默认 true，自带硬件门故正确但不受控），
  h263 缺省关（no_std fail-closed，静默标量）。mpeg2v/theora 同 h263
  （no_std 下 fail-closed OFF，特性关时调用即 no-op）。接上线后六家皆受控
  （单比特跟随全局，无独立开关可拧）。

## AVC 解码 SIMD（已 vendor + no_std 自 port，2026-10-21）

轮子（`rusty_h264-decoder`）host 上开 `asm` 纯 Rust 可移植 SIMD 已证提速
（720p census 全进内核：wide AVX2 + base SSE2，scalar 零调用；
baseline 1.55×、main/CABAC 1.23×，与 ffmpeg 像素级一致）。
固件侧 `asm` 开：上游 accel 箱（`rusty_h264-accel` 0.16.1）vendor 到
`third_party/rusty_h264-accel` 后自 port（`std` 默认开保上游行为、
关掉即 `#![no_std]` + `alloc`，特性名 `census`/`knobs` 与上游一致），
经根 `[patch.crates-io]` 截到 `common` 的可选依赖上；
decoder 本体保持 crates.io 外部箱不动（`default-features=false` +
`libm`）。前代两道门（std 撞车 E0152 / 裸 flags 下 LLVM ICE）已消解，
终链为准绳（`cargo check` 能过≠能用，见 vendor `VENDOR.md` 固件门契约）。

接线见上表 `h264` 行：无独立开关，直接跟随全局（`scalar`=关/其余=开），
开则解码取轮子 max（SSE2 保底；AVX2 在 no_std 下恒最高档，真门是开机
`resolve_accel` 推下来的，轮内无第二套探）；开机已按需开过 XSAVE，
AVC 首解前（`avc_probe`）再幂等保一次，防 OVMF 默认 CR4.OSXSAVE=0 下
XGETBV 的 `#UD`。串口 `accel: h264 decode isa = …` 行供 QEMU 矩阵断言。

## 约束沉淀

- `asm!` 显式向量寄存器优先于内在函数（防 LLVM 标量化，血泪见 `hw_bench.rs` 注释）。
- 浮点向量不执行：软浮点历史下会被标量化/算错常量，只报 CPUID（`hw_bench` 矩阵）。
- 非暂存（nt）store 计 `sfence`；`nt` 要求天然对齐（见 `gop_simd` 门控）。
- AVX/AVX2 跑前必须过 `check_avx_support`（CR4.OSXSAVE + XCR0），AVX512 另要
  `AVX512F` 硬件位 + XCR0 五位全开，缺一个执行就 `#UD`。
- 固件常不开 CR4.OSXSAVE：开机采固件原状态（`capture_firmware_osxsave`），
  `ensure_xsave_enabled` 按需自开（CPL0，可幂等），各报告引用原状态免污染。
