# Vendored `h263-rs` (+ `h263-rs-yuv` + `h263-rs-deblock`) — no_std port (scalar)

- 上游：https://github.com/ruffle-rs/h263-rs ，2026-10-21 master
  （`git clone --depth 1` 全树；三箱均为 0.1.0，未发 crates.io——
  crates.io API 查无此箱，只能 vendor 或 git 依赖；需源码级 no_std 改造，
  故 vendor）。
  MIT OR Apache-2.0（`LICENSE.md` 见本目录）。
- 本目录是“上游全树 + no_std 移植层”，不是 fork 上游仓库：
  上游目前**不支持 no_std**（`std::io::Read` 公有泛型界、`std::collections`、
  `thiserror`/`num-traits` 默认带 std、`wide` 无条件 SIMD，
  2026-10-21 实测 `cargo check --target x86_64-unknown-uefi` 能过只是因为
  本机新工具链自带 UEFI 的 `libstd`，链接/运行仍不可用，且本仓禁固件侧 `std`）。
  等上游自己长出 no_std 分层就切回（上游若发布 crates.io 则优先直接依赖），
  把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 2026-10-21 master）

1. 三箱各加 `std` 特性（默认开，行为与上游完全一致）；
   关掉即 `#![no_std]` + `alloc`（`extern crate alloc` + 按文件 `use alloc::…`）。
2. `h263`：
   - `thiserror` / `num-traits` 改 `default-features = false`
     （`bitflags` 无默认特性，不用动）；
     `std::cmp/ops/result` → `core`，`Vec/VecDeque/vec!` → `alloc`（机械）。
   - `HashMap<u16, _>` → `alloc::collections::BTreeMap`
     （引用表常驻 ≤2 条目，顺序无关，零新依赖；`remove_entry` 改 remove+重插）。
   - 公有 `R: std::io::Read` 泛型界全部改写为自研最小 trait
     `parser::ByteSource`（仅 `read_exact`，见 `reader.rs`）：
     std 下 blanket impl 给所有 `T: std::io::Read`（上游行为零变），
     no_std 下 `impl for &[u8]`（固件形：整帧已在 RAM）。
     这是与上游唯一的公有 API 分叉点，合流时重审。
   - `error.rs`：`UnhandledIoError(IoError)` + `From<IoError>` 做 `std` 门控；
     no_std 侧补 `UnexpectedEof` 单元变体（固件读数只会短读，
     信息量与 `UnexpectedEof` io kind 等价，`is_eof_error` 双形同义）。
   - 浮点方法整数化（本机工具链实锤：`ceil/floor/round/trunc/sqrt` 在
     `no_std` 下不在 `core` 内）：`div_ceil`（chroma/macroblock 行列）、
     `HalfPel::from(f32)` 手写 floor（含饱和守卫，与 `as` 饱和一致）、
     测试助手的 `round` 改 `+0.5` 截断（非负 lanes 等价）。
     改的是全形共享代码（非 cfg 双份），std/no_std 同一路。
3. `yuv`：`wide` 改可选（`bytemuck` 零默认特性保留）；
   2026-10-21 首版是 std-only（`std = ["wide/std"]`）+ 约 50 行标量 twin
   `yuv_to_rgba_4x`（同 16.16 定点系数、同舍入、同钳制、同 LE 交错，
   操作数恒远小于 `i32` 上界，无 wrapping 分歧；纯整数，无需 libm）。
   2026-10-21 SIMD 移植（选型 A，见下 §SIMD）：新增 `simd` 特性
   （`simd = ["dep:wide"]`，`std = ["simd", "wide/std"]`）——wide 1.7.1
   本身 `#![no_std]`，其 `std` 特性只加速 f32/f64 `sqrt`（本核纯整数，
   永不触及），故 SIMD 核同样进固件；标量 twin 改名
   `yuv_to_rgba_4x_scalar` 后恒编译，SIMD 核改名 `yuv_to_rgba_4x_simd`
   （`simd` 门控），分发器 `yuv_to_rgba_4x` 按运行时开关选路。
   `bytemuck::cast_slice` 双形通用。
4. `deblock`：`wide` + `itertools` 改可选（基线
   `default-features = false + use_alloc` 保 no_std 编译）；
   2026-10-21 首版是 std-only（`std = ["wide/std", "itertools/use_std"]`），
   `simd_impl` 整 mod + `izip` 八行并行循环做 `std` 门控，no_std 走标量
   twin。2026-10-21 SIMD 移植（选型 A）：新增 `simd` 特性
   （`simd = ["dep:wide", "dep:itertools"]`，
   `std = ["simd", "wide/std", "itertools/use_std"]`），门控全部
   `std` → `simd`；标量 twin 抽成恒编译 helper（`deblock_horiz_scalar` /
   `process_vert_quartet` + `process_vert_octet`，`vert` 同 chunk 起点
   x=2、同 ABCD 列 +4..=7，覆盖与 SIMD 路逐位一致），分发器按运行时
   开关选路（开 = 上游混合路 verbatim，关 = 全标量）。
   `use std::ops::Shr` 改 `core::ops::Shr`（双形通用，首版已做）。
   `simd`+无 `std` 组合**现已支持**（固件 SIMD 形；
   首版“明确不支持”的结论被本轮推翻，wide 永不进固件的前提不成立）。
5. `[[test]]` 无（单测全是 `#[cfg(test)]` 内联，host 双形都跑）；
   `readme` 指向保留的上游 README；上游 `PKGBUILD` / `rustfmt.toml` /
   `CODE_OF_CONDUCT` / `CONTRIBUTING` / `.github` 未 vendor（只留源码三箱 +
   工作区根 + `LICENSE.md` + 上游 README 改名 `README.upstream.md`）。
6. host 单测新增 `yuv420_to_rgba_dual_path_reference`
   （与上游测试同文件）：独立逐像素整数参考实现，`cargo test` 与
   `cargo test --no-default-features` 双形同断言，锁死 wide 路与标量路
   逐字节一致（含奇宽 remainder 路）。
   SIMD 移植新增双路互锁单测（`#[cfg(feature = "simd")]`，三形同跑：
   默认 / 无默认+`simd` / 无默认+`std`）：`yuv` 的
   `test_yuv420_to_rgba_simd_scalar_agree`（宽 1/2/3/4/5/7/8/9 × 高 5，
   开关各跑一遍逐字节比对）与 `deblock` 的
   `test_deblock_simd_scalar_agree`（7×20/8×8/9×9/10×10/11×17/16×9/24×24
   × strength 1/4/8/12）；既有 `test_deblock` 精确期望双路下原样通过
   （默认态：std 形走 SIMD 混合路、无默认形走全标量）。
7. `[accel]` 单比特开关接线（本轮只做到 vendor 侧，固件调用点随
   QEMU h263-probe 另立项）：`yuv::bt601::set_simd_enabled(bool)` 与
   `deblock::deblock::set_simd_enabled(bool)` 各一个 `Relaxed AtomicBool`
   （core 内置，零新依赖），无 `simd` 特性时为 no-op（调用方无需 `cfg`）；
   默认 ON 当且仅当 `std`（上游行为：恒 SIMD），no_std 下 fail-closed
   （OFF，直至固件 opt-in）。固件契约：`resolve_accel` 之后（`ensure_xsave_enabled`
   已幂等执行，见 `src/diag/common.rs`），按
   `accel.level == scalar ? off : on` 置位**两个**开关；本箱永不自研
   CPU 检测（无 `is_x86_feature_detected!`、无 `__cpuid` 内联第二套，
   树内 grep 可证），只服从开关。开关只是配置语义：两核只发
   SSE2 基线整数指令（x86-64 psABI  guaranteed，无 XSAVE/XCR0 依赖，
   release asm 已验零 VEX），错置位只影响哪条路跑、不影响正确性
   （双路逐字节一致已锁死）。

## 选型 A（wide 去 std 化）证据（2026-10-21，B 路 asm! 重写未采用）

- wide 1.7.1 首行即 `#![no_std]`；`std` 特性只做一件事：
  `extern crate std` 给 f32/f64 `sqrt` 提速（`src/lib.rs` 特性文档原话）。
  本箱只用整数向量（`i32x4`/`u8x16`/`i16x8`），`src/*x*.rs` 的 `std`
  门控只出现在 `f32x4_`/`f64x2_`，整数核零触及。
- 传递链全 no_std：`safe_arch 1.2.0`（wide 的 x86/x86_64 依赖）
  `#![no_std]` 且默认特性为空；`bytemuck` 无默认特性（core-only）；
  `itertools` 以 `use_alloc` 为基线即 no_std（`use_std` 只叠加）。
- wide 内部**无运行时 CPU 检测**：`is_x86_feature_detected`/`__cpuid`
  零命中，只有编译期 `#[cfg(target_feature)]` 分支；x86_64 下整数核
  落到 SSE2 基线，无条件可执行——没有需要门控掉的“第二套检测”。
- `x86_64-unknown-uefi` + 本仓 flags（`-soft-float,+sse,+sse2`）下
  三箱 rlib 一次编过，**无 LLVM ICE**（与 `accel` 箱裸 UEFI 下 ICE 的
  血泪对照：wide 是纯 `core::arch` 包装，无裸 `asm!`，不触发那条路径）。
- release asm 实锤向量化、零标量化：yuv 核（`movdqa/pshufd/paddd/
  punpckldq/punpcklbw/pxor/movdqu`）、deblock 核（`psubw/punpcklwd/
  pmaxsw/pxor/paddw/psraw/packuswb/pcmpgtw`），且**零 VEX/AVX 指令**。
- 终链验证：纯 core 的 `no_std` UEFI bin（自带 bump allocator +
  `panic_handler`）链接三箱固件 SIMD 形（`--no-default-features` +
  `simd`）一次通过，拉通解码管线（含标量 f32 IDCT）+ 双核开关；
  f32 走 core 硬浮点（x86_64 SSE2，软浮点已摘），**无需 libm**。
- 浮点向量不执行：沿用本仓纪律，wide f32/f64 类型本箱零引用。

## 记死版本（2026-10-21 vendor 时解析，根 Cargo.lock 不入库，口径以此为准）

- `h263-rs 0.1.0` / `h263-rs-yuv 0.1.0` / `h263-rs-deblock 0.1.0`（上游未发版）
- `bitflags 2.13.2` / `thiserror 2.0.21` (+ `thiserror-impl 2.0.21`) /
  `num-traits 0.2.19` (+ `autocfg 1.5.1`) / `wide 1.7.1` /
  `bytemuck 1.25.2` / `itertools 0.15.0` (+ `either 1.19.0`) /
  `safe_arch 1.2.0`（首版记为“host-only 不进固件”；SIMD 移植后更正：
  它是 wide 在 x86/x86_64 下的 `#![no_std]` 传递依赖，**进固件**，
  无默认特性，安全）
- SIMD 移植（2026-10-21）**零新增依赖**：`simd` 特性只以无 `std` 形态
  激活上表已有依赖（`wide` 不带 `std`、`itertools` 仅 `use_alloc`）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点；
合上游新版时按注释逐处重放。`src/bin/` 上游本来就没有；
`tests/` 目录上游本来就没有（只有内联单测，随树保留，host 双形回归用）。

## 帧输出与校验和口径（h263-probe/diag 接入前定死，2026-10-21）

- 解码帧：`DecodedPicture::as_yuv()` → `(Y, Cb, Cr)` planar `u8`
  （chroma `ceil(w/2)×ceil(h/2)`，8-bit 4:2:0 排布）；
  `yuv420_to_rgba(y, cb, cr, w)` → RGBA8888 `Vec<u8>`（Flash 式最近邻色度）。
- 校验和口径：**md5(Y 平面字节) + md5(RGBA 输出字节)**（双 md5，
  对标 hevc-probe 的“整文件 md5 + Y 和”——h263 无容器整文件概念，
  故两路输出各取其一；色度不单独计，RGBA 已含其信息）。
- 承诺范围：**只承诺 Sorenson Spark / 基线 H.263**
  （`DecoderOption::SORENSON_SPARK_BITSTREAM`），上游自述其它风味没测过。

## 本仓约定豁免

- 这是第三方上游代码（MIT/Apache-2.0，版权归 Michael Welsh 等贡献者），
  **不受**“单文件 ≤1000 行”约束——只要求新增的移植层自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`
  （`--all` 会连带 path 依赖一起查；见根 AGENTS.md「双构建」注释）。

## 已知风险（2026-10-21）

1. 上游**无 MSRV / rust-version 声明**，CI 只跟最新 stable：
   合流升级时重审依赖版本（wide edition 2024 + rust-version 1.89 是当前天花板）。
2. `ByteSource` 公有泛型界是与上游的永久分叉点（host 侧 `File`/`&[u8]`
   调用方经 blanket impl 零改，固件侧只传 `&[u8]`）。
3. f32 标量 IDCT 精度/性能待真机验证（QEMU h263-probe + 帧率 gate 后继项）。
4. 运行时开关默认 no_std 下 OFF：固件侧忘记置位就跑解码，结果正确
   （标量路逐字节一致）但吃不到 SIMD；h263-probe 立项时把“开关状态”
   打进串口日志，QEMU 矩阵可断言（`[accel]` 非 scalar 档必须观测到
   SIMD 开）。
5. `izip!` 在 `use_alloc` 下的 no_std 可用性是 itertools 0.15 的实现
   细节（本轮 host + UEFI 双编译已证）；升级 itertools 大版本时重验
   `vert` 的八行并行循环编译与双路一致单测。

## 播放器 `format` 接线状态（2026-10-21）：已接通（`video.h263` 裸流）

- `[play] format = "h263"` 固件 `Clip::from_h263_*` + GUI 同构 schema 全通；
  样片 `src/diag/data/video.h263`（ffmpeg testsrc 128x96×10 帧，QCIF 基线）。
- 上游 PTYPE 行为修正（`parser/picture.rs`，`VENDOR(uefi-player):` 标）：
  基线 `PictType` 首比特 0=INTRA(I)/1=INTER(P)（ITU-T H.263 §5.1.2 Table 2，
  ffmpeg 自家解码器同式），上游写反导致一切基线 I 帧被判 P、无参考帧可拷、
  首帧即 `UncodedIFrameBlocks`。修正后 ffmpeg 基线流 10/10 解码，
  与 ffmpeg 解 YUV 逐字节比对：首帧纯 intra 差 1.7% max 1（整数 IDCT 舍入），
  P 帧正常漂移 max 3（MC+IDCT 累积，容差口径 max|Δ|≤3，见下）。
  箱内单测 37 全绿（25+5+7，解析映射无单测锁死）。
- 校验和口径：md5(Y)+md5(RGBA)（见 §帧输出与校验和口径），
  另接受 max|Δ|≤3 的 ffmpeg 容差比对（P 帧漂移预期内）。
- 仍只承诺 Sorenson Spark / 基线（`DecoderOption::empty()`，标准 PTYPE 路）；
  Sorenson 变体流另议（FLV 拆包不在本仓）。
