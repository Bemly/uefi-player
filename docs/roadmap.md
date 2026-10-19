# uefi-player 路线图

> 口径：x86 引导加载器（基于 sprout 扩展）+ SIMD 正式矩阵 +
> UEFI 播放器 + 引导动态壁纸 + 跨平台配置 GUI。
> sprout（`third_party/`）只读，不 fork、不改。

## P0 文档骨架（先行）

- [x] `AGENTS.md`：Agent 约束（含轮子优先、双构建隔离）
- [x] 本文档：总计划
- [x] `docs/arch.md`：`src/{main,menu,graphics,video,diag}` + 未来 `src/boot/` 关系，
  GOP 独占模型、看门狗、MP 模型
- [x] `docs/bootloader.md`：sprout 扩展设计（复用 crate 清单、entry/BLS/chainload 接入点）
- [x] `docs/simd.md`：`accel.level` 矩阵生效表 + `hw_bench/gop_simd` 诊断沉淀
- [x] `docs/player.md`：`video.qois` 格式与 `VideoMemoryRaw` 管线
- [x] `docs/wallpaper.md`：双模式动态壁纸设计
- [x] `docs/config.md`：`player.toml` 完整 reference（由 `config.rs` 头注释升格）

## P1 引导（基于 sprout 扩展 → 全功能对齐，用户拍板）

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

## P2 SIMD 矩阵转正

- `config::accel_level()` 从“只记录”变为真分发：
  `graphics::upload_frame` / `video::{blt_loop, mp_draw}` / `gop_simd` 变体按档位选路。
- bench 只出数（`HWBENCH`），矩阵做选择；`auto` = CPUID/XCR0 探最高可用。
- 验收：`player.toml` 切档位，播放帧率/报告可复现对应变化。

## P3 播放器

- 保留双路径（Blt 单线程 / Direct 多核），补格式校验 + 掉帧策略，
  真机错位问题（`video.rs` TODO）闭环。
- 验收：1080p 循环播 + OSD（FPS/FT/Margin）正常。

## P4 动态壁纸（菜单背景 + 引导过渡都要，播放器 `Clip` 的薄调用层）

> 片源格式矩阵（`[wallpaper] format`）：默认 `raw` 零解码直搬（首帧无抖动）；
> `qois` 读 `wallpaper/wall.qois`，开机预解码（与播片同代价）。
> 解码全部复用 P3 的 `Clip`，壁纸只剩开关 + 帧率 + 循环语义。

- 格式：U盘 `wallpaper/` 下 `frameNNNN.raw`，每帧 `宽×高×4` 字节 BGRA 直存
  （首版定死 1920x1080，和视频同分辨率，免缩放分支；转制用 ffmpeg 一行：
  `ffmpeg -i in.mp4 -s 1920x1080 -pix_fmt bgra -f rawvideo frame%04d.raw`）。
- 配置（`player.toml` 新增 `[wallpaper]`，GUI 同步加开关页）：
  `enable = false`（默认关，老机器显存带宽兜底）、`fps = 15`（菜单背景低帧即可）、
  `frames = 0`（0 = 全播，B 过渡用 N 帧封顶）。
- A 壁纸待机：`idle::run` 待机时全屏按 `fps` 循环播（`popup_freeze=false`
  则弹窗时行区继续播）；任意键停并弹启动选项。
- B 引导过渡：选 entry 后、chainload 前播 N 帧/固定时长（依赖 P1 `src/boot/` 先有入口）。
- 验收：开关两档在 QEMU + 真机各测一遍；开档下按键响应无可感知延迟。

## P5 跨平台配置 GUI（host）

- [x] `tools/config-gui`：eframe 0.35 + wgpu 驱动 + egui_glass 液态玻璃，
  可视化编辑 `player.toml`（accel/render/blt_op/tune），与固件 `config.rs` 同 schema。
- U 盘根目录的 `player.toml` 拖进去改完存回，不用手写 TOML。
- 验收：`cargo run --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"`，
  改完的文件被固件正常解析（`cfg:` 无警告）。

## 验证门禁

```bash
cargo build --target x86_64-unknown-uefi && cargo clippy --target x86_64-unknown-uefi && cargo fmt --all --check
cargo check --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
# 黑盒矩阵（GUI 表 + QEMU 矩阵，改功能必跑，见 AGENTS「测试 100% Rust」）
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
```
