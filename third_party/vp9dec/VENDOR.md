# Vendored `vp9dec` — no_std port (scalar + AVX2 SIMD)

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
2. 口径：无 std 时线程并行恒走顺序路（上游官方 315/315
   门控本来就双跑这两条路，bit-exact，丢速度不丢正确性）；
   AVX2 在 std 与 no_std 下都可用（见下条），不再恒走标量。
3. `simd` 已 port 到 no_std（2026-10-21 落地）：内核模块 + 重导出整体改
   `#[cfg(target_arch = "x86_64")]`（内核是纯 `core::arch::x86_64` 整数内在，
   零依赖，std/no_std 通吃）；分发点（predict/loop_filter/residual）门控
   同步收窄为 arch-only。
   - `avx2_enabled()`：std 侧沿用上游语义（`VP9DEC_NO_SIMD` + 
     `is_x86_feature_detected!("avx2")`，OnceLock 缓存）再与
     `[accel]` 开关取与；no_std 侧 = 开关 × 固件 CPUID/XCR0 门
     （`ensure_xsave_enabled` 语义 + `check_avx_support` 门 +
     AVX2 硬件位，`AtomicU8` 三态缓存；env 恒读未设置，h264-common knob 主义）。
   - `[accel]` 单比特开关（`simd::set_simd_allowed`，默认开）是固件
     `config::copy_width()` 的接收端：`scalar`→关、其余→开；
     显式超硬件由固件 `resolve_accel` 钳住，这里只做 min(配置,硬件)
     （开关=配置半，`avx2_enabled` 硬件门=硬件半）。不读第二遍 player.toml。
   - UEFI 纪律（根 AGENTS.md + docs/simd.md AVC 节）：AVX2 跑前必过
     CR4.OSXSAVE+XCR0 门；no_std UEFI 目标首探时幂等自开 XSAVE
     （`simd.rs::ensure_xsave_enabled`，固件同名函数的语义镜像，
     跨 crate 调不到——防 OVMF CR4.OSXSAVE=0 下 XGETBV `#UD`；
     固件开机 `resolve_accel` + 首解前各保一次，这里是轮子自保第三道）；
     host no_std 只读不写 CR4/XCR0（用户态写必 `#GP`）。
     内核审计：整数向量-only（无浮点向量可报），plain load/store
     （无 nt、无需 sfence/对齐门控）——见各内核文件头。
   - 线程两 driver 仍 std-only（见下条），no_std 固件是单线程顺序解码 +
     SIMD 内核，行为与官方 sweep 的 SIMD 臂一致。
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
- SIMD 落地回归（2026-10-21，host，harness 在 `/tmp/vp9check` 未进仓）：
  自制 8 段（libvpx-vp9 编码：p0 320x240/640x480/720p/176x144/tiles、
  p2 10-bit 420、p3 444 10-bit）：SIMD == 标量（knob）== `VP9DEC_NO_SIMD=1`
  == ffmpeg `extractplanes=y -f md5`，三路全等；
  上游官方向量 12 段复用（resize/SVC/tiles-fp/奇尺寸/subpel/
  show-existing/droppable/p1-422/p2-10bit/p2-12bit/p3-444）：
  SIMD 与标量逐帧 md5 全等，且逐帧等于官方 `.ivf.md5`（YUV 拼接口径）。

## 本仓约定豁免

- 这是第三方上游代码（MIT，版权归 ChanyaVRC），**不受**
  “单文件 ≤1000 行”约束（`loop_filter.rs` 等本就超限）——只要求新增的移植层
  自身保持小而集中。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`（`--all` 会连带 path 依赖
  一起查；见根 AGENTS.md「双构建」注释）。

## 播放器 `format` 接线状态（2026-10-21）：已接 `Clip`

- `[play] format = "vp9"`：片源 `1080p/video.ivf`（32 字节头 + 12 字节帧头 + 负载），
  `Clip::from_vp9_bytes/file` 经本箱 `ivf::IvfReader` 拆帧 +
  `Decoder::decode_frame` 逐帧解码 → YUV420 转 BGRA 进同一 `Clip`，
  三条播放路零分支。
- 只要显示帧（隐藏 altref 无画面，直接跳过，不记坏）；只收 8-bit 4:2:0
  （`PlaneData::U8` + `subsampling == (1,1)`，其余记数跳过；10/12-bit
  固件用不上，先不验，见上节回归口径）。样片
  （`src/diag/data/vp9_probe.ivf`，testsrc 160x120×10 帧，host 已验
  `shown=10 luma_sum=23989636`）。QEMU 矩阵 `video-vp9` 播此样片断言
  `video: fmt=Vp9` + 播放路径。
