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
3. `yuv`：`wide` 改可选 std-only（`std = ["wide/std"]`，`bytemuck`
   零默认特性保留）；no_std 走约 50 行标量 twin
   `yuv_to_rgba_4x`（同 16.16 定点系数、同舍入、同钳制、同 LE 交错，
   操作数恒远小于 `i32` 上界，无 wrapping 分歧；纯整数，无需 libm）。
   `bytemuck::cast_slice` 双形通用。
4. `deblock`：`wide` + `itertools` 改可选 std-only
   （`std = ["wide/std", "itertools/use_std"]`，基线
   `default-features = false + use_alloc` 保 no_std 编译）；
   `simd_impl` 整 mod + `izip` 八行并行循环做 `std` 门控，
   no_std 走标量 twin（`horiz` 逐像素全覆盖；`vert` 同 chunk 起点
   x=2、同 ABCD 列 +4..=7，覆盖与 SIMD 路逐位一致）。
   `use std::ops::Shr` 改 `core::ops::Shr`（双形通用）。
   `simd`+无 `std` 组合**明确不支持**（wide 永不进固件）。
5. `[[test]]` 无（单测全是 `#[cfg(test)]` 内联，host 双形都跑）；
   `readme` 指向保留的上游 README；上游 `PKGBUILD` / `rustfmt.toml` /
   `CODE_OF_CONDUCT` / `CONTRIBUTING` / `.github` 未 vendor（只留源码三箱 +
   工作区根 + `LICENSE.md` + 上游 README 改名 `README.upstream.md`）。
6. host 单测新增 `yuv420_to_rgba_dual_path_reference`
   （与上游测试同文件）：独立逐像素整数参考实现，`cargo test` 与
   `cargo test --no-default-features` 双形同断言，锁死 wide 路与标量路
   逐字节一致（含奇宽 remainder 路）。

## 记死版本（2026-10-21 vendor 时解析，根 Cargo.lock 不入库，口径以此为准）

- `h263-rs 0.1.0` / `h263-rs-yuv 0.1.0` / `h263-rs-deblock 0.1.0`（上游未发版）
- `bitflags 2.13.2` / `thiserror 2.0.21` (+ `thiserror-impl 2.0.21`) /
  `num-traits 0.2.19` (+ `autocfg 1.5.1`) / `wide 1.7.1` /
  `bytemuck 1.25.2` / `itertools 0.15.0` (+ `either 1.19.0`) /
  `safe_arch 1.2.0`（wide 的 host-only 传递依赖，不进固件）

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
