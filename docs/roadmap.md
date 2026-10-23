# uefi-player 路线图

> 口径：x86 引导加载器（基于 sprout 扩展）+ SIMD 正式矩阵 +
> UEFI 播放器 + 引导动态壁纸 + 跨平台配置 GUI。
> sprout（`third_party/`）submodule pin 死、不 fork，只通过 `patches/` 叠加（见 `AGENTS.md`）。

## P0 文档骨架（先行）

- [x] `AGENTS.md`：Agent 约束（含轮子优先、双构建隔离）
- [x] 本文档：总计划
- [x] `docs/arch.md`：`src/{main,menu,graphics,video,diag}` + `src/boot/` 关系，
  GOP 独占模型、看门狗、MP 模型（已按 `src/ptui/` + `src/font/` 与 p→TUI 路由改写）
- [x] `docs/bootloader.md`：sprout 扩展设计（复用 crate 清单、entry/BLS/chainload 接入点）
- [x] `docs/simd.md`：`accel.level` 矩阵生效表 + `hw_bench/gop_simd` 诊断沉淀
- [x] `docs/player.md`：`video.qois` 格式与 `VideoMemoryRaw` 管线
- [x] `docs/wallpaper.md`：双模式动态壁纸设计
- [x] `docs/config.md`：`player.toml` 完整 reference（由 `config.rs` 头注释升格）

## P1 引导（基于 sprout 扩展 → 全功能对齐，用户拍板；已落地）

- 新增 `src/boot/`，调用 `edera-sprout-config` 解析 + `edera-sprout-bls`
  枚举 + `edera-sprout-eficore` chainload + `edera-sprout-boot`（经 `patches/`
  叠 `[lib]` 后直调 `boot_counter` 等）。
- `registry.rs` 加 boot entry，与现有 `run_menu` 合流（`Shared` 形态）。
- 验收：QEMU 下能列 BLS entry 并 chainload，sprout submodule 除 `patches/` 叠加外零修改。
- 全功能对齐清单（`src/boot/` 薄适配直调，UI/配置保持本仓，见 AGENTS）：
  - [x] 本卷 BLS 枚举 + PE chainload（`entries.rs` + `chainload.rs`，QEMU 矩阵已验证）
  - [x] 跨盘 chainload（目标卷设备句柄拼路径 + 跨卷读；`boot-chainload`/`boot-windows-crossvol` 已验证）
  - [x] 多卷自动发现（`detect.rs`：逐卷 BLS/UKI/Linux 内核对/Windows/Shell/OpenCore）
  - [x] Linux 直启（kernel + initrd 拼接 + options + devicetree，经
    `eficore::media_loader` 交接；QEMU 只验发现列出，不真起内核）
  - [x] BLS 全字段透传（options/多 initrd 拼接/`devicetree` 键/`arch` 过滤/`uki` 键）
  - [x] boot counting 消费（启动尝试即递减 tries，失败 warn 不挡）
  - [x] `[boot]` 配置 + GUI 开关页 + QEMU 矩阵覆盖（第二卷假 Windows/BLS/Linux/OpenCore）

## P2 SIMD 矩阵转正（已落地）

- `config::accel_level()` 已真分发：`graphics::upload_frame` / `video::{blt_loop, mp_draw}` /
  `gop_simd` 变体按档位→位宽（`CopyWidth`：标量 U64 / SSE 系 X128 / AVX 系 V128 /
  AVX2 系 Y256 / AVX512 系 Z512，显式超硬件警告+钳住）选路；`auto` = CPUID/XCR0 探最高可用。
- bench 只出数（`HWBENCH`），矩阵做选择；解码 SIMD 无独立开关：
  六家（hevc/vp9/h263/h264/mpeg2v/theora）单比特跟随全局搬运位宽
  （`level=scalar` 则关，其余开），见 `docs/simd.md`。
- 验收：`player.toml` 切档位，播放帧率/报告可复现对应变化（矩阵 `video-decode-*` 断言
  `honored`/`deferred` 行）。

## P3 播放器 + TUI（已落地）

- 播放：双路径保留（Blt 单线程 / Direct 多核）+ 格式校验 + 掉帧策略闭环；
  `[playlist]` 逐项自动检测统一落 `Clip`（九档检测序，见 `docs/player.md`），
  `[play] feed` 五档只切拿帧方式，`[decode.<fmt>]` 八表调并发/缓冲/供片；
  真机错位问题闭环中（坏帧计数 + 空集报错 + 几何校验，串口留数；根因仍待真机抓包）。
- TUI（`src/ptui/`）：待机/弹窗按 `p` 进九宫首页（播放/加速/渲染/调度/解码/
  显示/测试/壁纸 + 返回），`q` 逐层回；六页调档只改 runtime 内存，
  只有壁纸页 `s` 落盘（只写 `[wallpaper]` 表）。
- 字体（`src/font/`）：TUI 点阵，英文 8x16 原表零改动 + 中文 16x16 子集转写
  31 字（Unifont 15.1.05，约 1KB）+ 度量冻结（8/16/行高 18，对齐 `WP_TOP`），
  缺字豆腐块不断行。
- 扫描（`src/ptui/media.rs`）：`media_dir`（`""`=根，单层非递归）枚举直属项 +
  tagged 探针只读识别，按字节序排；`play_path` 与列表同管线。
- 验收：1080p 循环播 + OSD（FPS/FT/Margin）正常；矩阵 `70-ptui.toml`
 （`ptui-smoke` 进页拨档往返 / `ptui-play-scan` 扫描播片 / `ptui-wallpaper-save`
  壁纸落盘）全绿。

## P4 动态壁纸（菜单背景 + 引导过渡都要，播放器 `Clip` 的薄调用层；已落地）

> 画廊唯一入口 `[playlist]`（`[[wallpaper.items]]` / `[wallpaper] loop` /
> `format` / `frames` 旧键已删除）：显示哪几个/多少个/什么顺序/怎么循环全由
> `[playlist]` 定；`[wallpaper]` 只留四键（`enable/fps/preload/popup_freeze`）。
> 解码全部复用 P3 的 `Clip`，壁纸只剩开关 + 画廊拼接 + 帧率。

- 拼接：`wallpaper::load` 逐项检测加载（恒 eager，坏项跳过 + warn），
  `preload` 按项封顶截断（0 = 不封顶），几何一致校验后全拼一个 `Clip`；
  空画廊/全坏 = 静默无壁纸（不 halt 流程）。
- 上屏三处全传 Clip 真实 `w/h` 走 `display.fit`（待机全屏 / 行区动画 /
  B 全屏过渡；`fit` 五档 + `align` 五档 + `bar` 留边，见 `docs/wallpaper.md`）。
- A 壁纸待机：`idle::run` 待机时全屏按 `fps` 循环播（`popup_freeze=false`
  则弹窗时行区继续播）；任意键停并弹启动选项（`p` 直进 TUI）。
- B 引导过渡：选 entry 后、chainload 前播画廊全长，播完 `drop(Screen)` 交权。
- 验收：开关两档在 QEMU + 真机各测一遍；开档下按键响应无可感知延迟。

## P5 跨平台配置 GUI（host；已落地，与 `[playlist]` 收敛同 schema）

- [x] `tools/config-gui`：eframe 0.35 + wgpu 驱动 + egui_glass 液态玻璃，
  可视化编辑 `player.toml`（accel/render/blt_op/tune），与固件 `config.rs` 同 schema。
- U 盘根目录的 `player.toml` 拖进去改完存回，不用手写 TOML。
- 验收：`cargo run --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"`，
  改完的文件被固件正常解析（`cfg:` 无警告）。

## 验证门禁

```bash
cargo build --target x86_64-unknown-uefi && cargo clippy --target x86_64-unknown-uefi && cargo fmt -p uefi-player --check
cargo check --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
# 黑盒矩阵（GUI 表 + QEMU 矩阵，改功能必跑，见 AGENTS「测试 100% Rust」）
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
```
