# uefi-player

**简体中文** | [English](README.md)

一个 `no_std` 的 x86-64 UEFI 应用：一个 `.efi` 同时是**引导加载器、视频播放器和引导动态壁纸**，在任何操作系统启动之前，直接在固件里播视频。另附一个跨平台的液态玻璃**配置 GUI**，用来编辑它的 `player.toml`。

- **引导加载器**：基于 [sprout](https://github.com/edera-dev/sprout) 的库。逐卷自动发现启动项（BLS、UKI、Linux 内核 + initrd、Windows、UEFI Shell、OpenCore）并链式启动。
- **视频播放器**：HEVC、AVC、VP9、H.263、MPEG-2、Theora 纯软解，外加 raw 和 QOI 帧序列。借固件的 MP 服务多核渲染。
- **SIMD 搬运矩阵**：按 CPUID/XCR0 自动选标量、SSE、AVX、AVX2 或 AVX-512 拷贝内核，也可以在配置里钉死档位。
- **动态壁纸**：既做启动菜单背景，也在交权给系统前全屏播一段过渡。
- **文本界面（TUI）**：中英文像素字体，开机即可调全部参数，不需要进系统。

## 下载

预编译产物在 [Releases](https://github.com/Bemly/uefi-player/releases) 页：

| 文件 | 说明 |
|---|---|
| `uefi-player-usb-x86_64.zip` | 解压到 FAT32 U 盘根目录即可：`EFI/BOOT/BOOTX64.EFI` + `player.toml` |
| `uefi-player-x86_64.efi` | 裸固件应用 |
| `UEFI-Player-windows-x64.exe` | Windows 版配置 GUI |
| `UEFI-Player-macos-x64.dmg` | macOS（Intel）版配置 GUI，打开后把应用拖进"应用程序" |
| `UEFI-Player-linux-x64.AppImage` | Linux 版配置 GUI，`chmod +x` 后运行 |

GUI 安装包都没有代码签名：
- **macOS**：第一次打开时右键点应用选"打开"，或执行 `xattr -dr com.apple.quarantine "/Applications/UEFI Player.app"`。
- **Windows**：首次运行 SmartScreen 会拦，点"更多信息"，再点"仍要运行"。
- **Linux**：部分发行版运行 AppImage 需要先装 `libfuse2`。

## 快速上手

1. U 盘格式化为 FAT32，把 `uefi-player-usb-x86_64.zip` 解压到根目录。
2. 把视频放在 `player.toml` 旁边，写进 `[playlist]`。转码用配置 GUI 最省事（见下文）。
3. 从 U 盘启动。需要关闭 Secure Boot，因为应用没有签名。

```toml
version = 1

[playlist]
loop = "loop_list"          # loop_list | loop_single | once

[[playlist.items]]
path = "video.mp4"          # 格式逐项自动检测

[wallpaper]
enable = true               # 菜单动态背景 + 引导前过渡
fps = "15"
```

完整配置说明见 [docs/config.md](docs/config.md)。

### 支持的媒体

每个播放列表项按路径自动识别：

| 格式 | 文件 |
|---|---|
| QOI 序列 | `*.qois`（`u32le 帧长 + QOI 帧` 串接） |
| 原始 BGRA | `*.raws`（帧首尾相接）或 `frameNNNN.raw`（1920×1080） |
| MP4 | `*.mp4`，按 HEVC > AVC > VP9 取第一条匹配的视频轨 |
| HEVC | `*.hevc`、`*.265`（Annex-B 裸流） |
| VP9 | `*.ivf` |
| H.263 | `*.h263` |
| MPEG-2 视频 | `*.m2v` |
| Theora | `*.ogv` |

视频编解码器只解 8-bit 4:2:0。识别失败的项会警告并跳过，不会中断播放。详见 [docs/player.md](docs/player.md)。

### 开机按键

| 场景 | 按键 | 作用 |
|---|---|---|
| 壁纸待机 | 任意键 | 弹出启动选项 |
| 壁纸待机 / 弹窗 | `p` | 进入 TUI |
| 弹窗 | `↑` `↓` `Enter` | 选择并启动条目 |
| 弹窗 | `f` / `r` / `s` | 进固件设置 / 重启 / 关机 |
| 任意位置 | `Tab` | 悬浮跳转菜单 |
| TUI | `q` / `Esc` | 返回上一层 |

在 TUI 里改的参数只对本次开机生效；只有壁纸页按 `s` 会写回 `player.toml`。

## 配置 GUI

`tools/config-gui` 是 `player.toml` 的桌面编辑器，基于 eframe/wgpu 和 [egui_glass](https://crates.io/crates/egui_glass)（液态玻璃）。它覆盖全部配置项，另外还有：

- **图库**：列出 `player.toml` 同目录的媒体，带封面缩略图，一键加入壁纸播放列表，可拖拽排序；
- **转码**：把任意片源转成固件能播的格式（MP4 封装的 AVC、HEVC、VP9，以及 H.263、MPEG-2、Theora、QOI、raw），有条件时 AVC 和 HEVC 走硬件编码（macOS 用 VideoToolbox，Windows/Linux 用 Vulkan Video）；
- 玻璃背后的实时视频背景。

直接打开 U 盘上的 `player.toml`，改完存回即可。

## 从源码构建

需要 Rust stable 1.92 或更新。UEFI target 会由 `rust-toolchain.toml` 自动安装。

```bash
git clone https://github.com/Bemly/uefi-player.git
cd uefi-player
git submodule update --init
# 叠加各解码器的 no_std 移植补丁（完整清单见 patches/README.md）
git -C third_party/sprout apply ../../patches/sprout-boot-lib.patch
# ……以及 patches/README.md 里另外七个补丁

# 固件 -> target/x86_64-unknown-uefi/release/uefi-player.efi
cargo build --release --target x86_64-unknown-uefi

# 配置 GUI（根目录 .cargo/config 把默认 target 钉成了 UEFI，须显式指定 host target）
cargo run --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
```

macOS 上全新编译 GUI 还需要带 `SDKROOT="$(xcrun --show-sdk-path)"`。Linux 需要装 X11、Wayland 和 Vulkan 的开发包，具体清单见 [release workflow](.github/workflows/release.yml)。

### 测试

测试全部用 Rust 写：

```bash
HOST="$(rustc -vV | sed -n 's/^host: //p')"
# GUI 黑盒矩阵（TOML 回写、旧文件兼容、钳制等）
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$HOST"
# QEMU 黑盒矩阵：启动真实固件、发按键、断言串口输出
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$HOST"
```

QEMU 矩阵需要 `qemu-system-x86_64` 和 OVMF，见 [tools/qemu-matrix/README.md](tools/qemu-matrix/README.md)。

### 发布

把根目录 `Cargo.toml` 的 `version` 改成当天日期（CalVer `YY.MM.DD`），推一个对应的 `v*` 标签。[release workflow](.github/workflows/release.yml) 会编出固件和三个平台的 GUI，发布成 GitHub Release。

## 目录结构

```text
src/               UEFI 固件（no_std）：boot/、video/、ptui/、font/、graphics/、diag/ 等
tools/             host 工具 workspace：config-gui、qemu-matrix、gui-pack、patch-sync、au-shape
third_party/       7 个 pin 死的 submodule（sprout + 6 家解码器），不 fork
patches/           叠在这些 submodule 上的功能补丁
docs/              设计文档（唯一口径）+ 真机报告（docs/reports/）
```

## 文档

- [路线图](docs/roadmap.md) · [架构](docs/arch.md) · [配置说明](docs/config.md)
- [引导](docs/bootloader.md) · [播放器](docs/player.md) · [动态壁纸](docs/wallpaper.md)
- [SIMD 矩阵](docs/simd.md) · [转码](docs/transcode.md)
- [贡献者与 Agent 约束](AGENTS.md)

## 许可证

项目本身尚未选定许可证。第三方组件沿用各自的许可证：`third_party/` 下的 submodule 跟随上游，内置的缝合像素字体（Fusion Pixel）为 OFL-1.1（见 `tools/config-gui/assets/OFL.txt`）。
