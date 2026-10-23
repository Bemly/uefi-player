# uefi-player

[简体中文](README.zh.md) | **English**

A `no_std` x86-64 UEFI application that is a **boot loader, a video player and an animated boot wallpaper** in one `.efi`. It plays video straight from the firmware, before any OS loads. It ships with a cross-platform liquid-glass **config GUI** for editing its `player.toml`.

- **Boot loader**: built on [sprout](https://github.com/edera-dev/sprout)'s libraries. It finds boot entries on every volume (BLS, UKI, Linux kernel + initrd, Windows, UEFI Shell, OpenCore) and chainloads them.
- **Video player**: software decoders for HEVC, AVC, VP9, H.263, MPEG-2 and Theora, plus raw and QOI frame sequences. Multi-core rendering uses the firmware's MP services.
- **SIMD copy matrix**: picks scalar, SSE, AVX, AVX2 or AVX-512 copy kernels from CPUID/XCR0, or from a level pinned in the config.
- **Animated wallpaper**: plays behind the boot menu and as a full-screen transition before handing off to the OS.
- **Text UI**: a Chinese/English pixel-font TUI for tuning everything at boot, with no OS required.

## Download

Prebuilt binaries are on the [Releases](https://github.com/Bemly/uefi-player/releases) page:

| File | What it is |
|---|---|
| `uefi-player-usb-x86_64.zip` | Unzip onto a FAT32 USB stick: `EFI/BOOT/BOOTX64.EFI` + `player.toml` |
| `uefi-player-x86_64.efi` | The bare firmware application |
| `UEFI-Player-windows-x64.exe` | Config GUI for Windows |
| `UEFI-Player-macos-x64.dmg` | Config GUI for macOS (Intel). Open it and drag the app into Applications |
| `UEFI-Player-linux-x64.AppImage` | Config GUI for Linux. Run `chmod +x`, then start it |

The GUI builds are not code-signed:
- **macOS**: right-click the app and choose *Open* the first time, or run `xattr -dr com.apple.quarantine "/Applications/UEFI Player.app"`.
- **Windows**: SmartScreen warns on first run. Choose *More info*, then *Run anyway*.
- **Linux**: the AppImage needs `libfuse2` on some distributions.

## Quick start

1. Format a USB stick as FAT32 and unzip `uefi-player-usb-x86_64.zip` onto its root.
2. Put your video next to `player.toml` and list it under `[playlist]`. Transcoding is easiest with the GUI (see below).
3. Boot the machine from the stick. Secure Boot must be off: the application is unsigned.

```toml
version = 1

[playlist]
loop = "loop_list"          # loop_list | loop_single | once

[[playlist.items]]
path = "video.mp4"          # format is detected per item

[wallpaper]
enable = true               # animated menu background + pre-boot transition
fps = "15"
```

The full reference is [docs/config.md](docs/config.md).

### Supported media

Each playlist item is detected from its path:

| Format | Files |
|---|---|
| QOI sequence | `*.qois` (`u32le length + QOI frame`, concatenated) |
| Raw BGRA | `*.raws` (frames back to back) or `frameNNNN.raw` (1920×1080) |
| MP4 | `*.mp4`, first video track in the order HEVC > AVC > VP9 |
| HEVC | `*.hevc`, `*.265` (Annex-B) |
| VP9 | `*.ivf` |
| H.263 | `*.h263` |
| MPEG-2 video | `*.m2v` |
| Theora | `*.ogv` |

The video codecs decode 8-bit 4:2:0 only. Items that fail detection are skipped with a warning, and playback continues. Details: [docs/player.md](docs/player.md).

### Keys at boot

| Where | Key | Action |
|---|---|---|
| Wallpaper idle | any key | Open the boot options popup |
| Wallpaper idle / popup | `p` | Open the TUI |
| Popup | `↑` `↓` `Enter` | Pick and boot an entry |
| Popup | `f` / `r` / `s` | Firmware setup / reboot / shut down |
| Anywhere | `Tab` | Floating jump menu |
| TUI | `q` / `Esc` | Back one level |

Changes made in the TUI only affect the current boot, except on the wallpaper page, where `s` saves them to `player.toml`.

## Config GUI

`tools/config-gui` is a desktop editor for `player.toml`, built with eframe/wgpu and [egui_glass](https://crates.io/crates/egui_glass) (liquid glass). It covers every config section and adds:

- a **gallery** of the media next to `player.toml`, with cover thumbnails, one-click add to the wallpaper playlist, and drag-to-reorder;
- a **transcoder** that turns any source into a format the firmware can play (AVC, HEVC and VP9 in MP4, H.263, MPEG-2, Theora, QOI and raw), encoding AVC and HEVC in hardware where it is available (VideoToolbox on macOS, Vulkan Video on Windows/Linux);
- a live video backdrop behind the glass.

Open the `player.toml` on your USB stick, edit it and save it back.

## Building from source

You need a stable Rust toolchain (1.92 or newer). The UEFI target is installed automatically by `rust-toolchain.toml`.

```bash
git clone https://github.com/Bemly/uefi-player.git
cd uefi-player
git submodule update --init
# apply the vendored no_std ports (full list in patches/README.md)
git -C third_party/sprout apply ../../patches/sprout-boot-lib.patch
# ... plus the other seven patches from patches/README.md

# firmware -> target/x86_64-unknown-uefi/release/uefi-player.efi
cargo build --release --target x86_64-unknown-uefi

# config GUI (the root .cargo/config pins the UEFI target, so pass the host target explicitly)
cargo run --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
```

On macOS, a fresh GUI build also needs `SDKROOT="$(xcrun --show-sdk-path)"`. On Linux, install the X11, Wayland and Vulkan development packages; the exact list is in [the release workflow](.github/workflows/release.yml).

### Tests

All tests are written in Rust:

```bash
HOST="$(rustc -vV | sed -n 's/^host: //p')"
# GUI black-box matrix (TOML round-trips, legacy files, clamping, ...)
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$HOST"
# QEMU black-box matrix: boots the real firmware, drives keys, asserts serial output
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$HOST"
```

The QEMU matrix needs `qemu-system-x86_64` and OVMF. See [tools/qemu-matrix/README.md](tools/qemu-matrix/README.md).

### Releasing

Bump `version` in the root `Cargo.toml` (CalVer `YY.MM.DD`) and push a matching `v*` tag. The [release workflow](.github/workflows/release.yml) builds the firmware and all three GUIs, then publishes them as a GitHub Release.

## Repository layout

```text
src/               UEFI firmware (no_std): boot/, video/, ptui/, font/, graphics/, diag/, ...
tools/             host workspace: config-gui, qemu-matrix, gui-pack, patch-sync, au-shape
third_party/       7 pinned submodules (sprout + 6 decoders), never forked
patches/           functional deltas applied on top of those submodules
docs/              design docs (the source of truth) + hardware reports in docs/reports/
```

## Documentation

- [Roadmap](docs/roadmap.md) · [Architecture](docs/arch.md) · [Config reference](docs/config.md)
- [Boot loader](docs/bootloader.md) · [Player](docs/player.md) · [Wallpaper](docs/wallpaper.md)
- [SIMD matrix](docs/simd.md) · [Transcoding](docs/transcode.md)
- [Contributor and agent rules](AGENTS.md)

The design docs are written in Chinese.

## License

[WTFNMFPL](LICENSE) (Do What The Fuck You Want To But It's Not My Fault Public License, v1). Do whatever you want with it; just don't hold the authors liable for anything that goes wrong.

Third-party components keep their own licenses: the submodules under `third_party/` follow their upstreams, and the bundled Fusion Pixel font is OFL-1.1 (see `tools/config-gui/assets/OFL.txt`).
