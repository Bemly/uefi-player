# SIMD 矩阵（`accel.level` 真分发）

> P2 落点：`config::copy_width()`。bench 只出数（`HWBENCH`），矩阵做选择；
> `auto` = CPUID/XCR0 探最高可用（`resolve_accel`，开机一次）。

## 档位 → 位宽

| `accel.level` | 搬运位宽 | 实现 |
|---|---|---|
| `scalar` | u64 步进 | `copy_block_u64`（纯标量，最兼容） |
| `sse2`/`ssse3`/`sse41`/`sse42` | 128 位 | `copy_block_x128`（`movdqu`，同宽） |
| `avx` | 128 位 VEX | `copy_block_v128`（`vmovdqu` + `vzeroupper`） |
| `avx2` | 256 位 | `copy_block_y256`（`vmovdqu` + `vzeroupper`） |
| `avx512` | 512 位 | `copy_block_z512`（`vmovdqu8` + `vzeroupper`） |

- 显式档位超硬件：警告 + 钳到硬件最大，不断 boot（`resolve_accel`）。
- `auto` 取硬件最大；`effective_accel` = `min(配置, 硬件)` 语义经 `copy_width` 落地。

## 选路位置

- `Screen::upload_direct` / `upload_frame_region`：按 `copy_width()` 分发
  （stride 感知逐行；连续整帧一次搬）。
- `video::mp_draw`：`PlayTask.copy` 快照位宽，BSP 镂空块 + AP 整块同走 `copy_block`。
- `blt_loop` / `direct_loop`：经 `upload_frame` 自动分发。
- `hw_bench` / `gop_simd`：保持全矩阵实测（bench 用，不参与选择）。

## AVC 解码（标量 shipped；SIMD 待上游解禁）

轮子（`rusty_h264-decoder`）host 上开 `asm` 纯 Rust 可移植 SIMD 已证提速
（720p census 全进内核：wide AVX2 + base SSE2，scalar 零调用；
baseline 1.55×、main/CABAC 1.23×，与 ffmpeg 像素级一致）。
固件侧暂用标量路，`asm` 开关有两道门：① 裸 UEFI 默认 flags 下 accel
codegen 报 LLVM ICE（本仓 `-soft-float,+sse,+sse2` 组合可解）；② 决定性：
accel 箱源码无条件 `use std::`（无 no_std 门），固件终链时 std 的
panic_impl 与 uefi 的 `panic_handler` 撞车（E0152；`cargo check` 能过、
终链才炸）。解禁条件：上游 accel 去 std 化。到时 `[accel]` 矩阵做单比特
开关（`scalar`=关，其余=开；开则解码取轮子 max，SSE2 保底、AVX2 看
CPUID/XCR0，开机 `resolve_accel` 已按需开过 XSAVE，AVC 首解前再幂等保一次，
防 OVMF 默认 CR4.OSXSAVE=0 下 XGETBV 的 `#UD`）。

## 约束沉淀

- `asm!` 显式向量寄存器优先于内在函数（防 LLVM 标量化，血泪见 `hw_bench.rs` 注释）。
- 浮点向量不执行：软浮点历史下会被标量化/算错常量，只报 CPUID（`hw_bench` 矩阵）。
- 非暂存（nt）store 计 `sfence`；`nt` 要求天然对齐（见 `gop_simd` 门控）。
- AVX/AVX2 跑前必须过 `check_avx_support`（CR4.OSXSAVE + XCR0），AVX512 另要
  `AVX512F` 硬件位 + XCR0 五位全开，缺一个执行就 `#UD`。
- 固件常不开 CR4.OSXSAVE：开机采固件原状态（`capture_firmware_osxsave`），
  `ensure_xsave_enabled` 按需自开（CPL0，可幂等），各报告引用原状态免污染。
