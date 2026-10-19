# QEMU 黑盒矩阵

QEMU 里跑真实固件，按 `matrix.toml` 逐 case 起虚拟机：塞 `player.toml` →
抓串口 → 发按键 → 断言。和 GUI 黑盒矩阵（`tools/config-gui` 的
`blackbox_matrix` 单测）合起来覆盖“GUI 改配置 → 落盘 → 固件生效”全链路。

轮子结论（见根 `AGENTS.md`「轮子优先」+「测试 100% Rust」）：搜过 crates.io
（qemu runner / uefi blackbox harness），只有底层 QMP 协议轮子，无开箱即用的
UEFI 黑盒矩阵工具；线束只是调 qemu 进程 + 读串口文件 + 发 monitor 按键，
标准库足够（`subprocess`/`TcpStream`/`fs`，`serde`/`toml` 与 config-gui 同版本），
故零新依赖，不建虚拟环境、不引 clap/regex/glob。系统工具
（`qemu-system-x86_64` / OVMF / `hdiutil`+`newfs_msdos` / `mtools`）只当子进程
调，不写测试逻辑（AGENTS 有令：测试必须 100% Rust）。
macOS 用 `hdiutil`（已验证），Linux 走 `mtools`（未在本机验证，只做分支）。

## 用法（与 config-gui 同式：显式 host target，根 `.cargo/config` 把默认钉死了）

```bash
# 全量（先编固件 debug，逐 case 跑；慢 case 默认跳过）
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"

# 只看列表 / 只跑一个 / 含慢 case / 留盘排查
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- --list
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- --case menu-smoke
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- --include-slow
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- --case video-mp --keep

# 固件双轨：默认 brew 瓶 DEBUG OVMF（带 ASSERT，门禁口径）；
# --ovmf-set release 全切 RELEASE（`assets/ovmf-release-x86_64-code.fd`），
# --ovmf-set both 双轨全跑（`-release` 后缀，比视频性能用它）。
# 单 case 也可在 matrix.toml 里钉 `ovmf = "release"`。
cargo run --manifest-path tools/qemu-matrix/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- --ovmf-set both --case video-mp
```

## 固件来源（轮子结论：不自编 edk2，直接用现成二进制）

- DEBUG：brew 瓶自带 `/usr/local/share/qemu/edk2-x86_64-code.fd`
  （Gerd Hoffmann 构建，`DEBUG_GCC5`，3.5MB；ASSERT 全开，门禁默认）。
- RELEASE：`assets/ovmf-release-x86_64-code.fd`（1.9MB，
  osdev0/edk2-ovmf-nightly `20261019T035340Z`，上游 edk2 `c1415771`，
  `build -b RELEASE`；无 ASSERT、启动更快）。
  二进制重 1.9MB，随仓库入库（video.qois 同先例）；edk2 许可证见上游
  `edk2-licenses.txt`（BSD 家族），与 brew 瓶同性质。
- 该换 release 跑全量上游直调（如 `actions::chainload` 的 `resolve_path`）
  时，先重跑 `--ovmf-set both` 全绿再谈（见 `docs/bootloader.md` 复用地图）。
- 分轨口径：`boot-*` case 全钉 `ovmf = "release"`（上游自家 dev 就是发行版
  bios.bin，这是它实测过的环境；切上游管线的前提）。其余 case 留 debug
 （ASSERT 抓 bug）。`--ovmf-set both` 仍可手动双跑抽查。

产物在 `tools/qemu-matrix/runs/<case>/`（串口全文 + verdict，默认清掉重跑；
`--keep` 留盘排查；已进 `.gitignore`，不入库）。
真机归档样本放根 `usb_reports/`，见该目录惯例。
测试视频用 `usb_reports/1080p/video.qois`（已入库，`needs_video` 的 case 会拷进镜像）。

## 矩阵设计

- 每个 case 独立镜像、独立 QEMU 进程（`player.toml` 内联在 `matrix.toml` 里），
  互不干扰；超时即 FAIL（固件 `run_menu` 永不返回，只能靠模式匹配 + 超时判定）。
- 按键经 human monitor（telnet）`sendkey` 注入；`pause_return` 的回车一律再补
  一个键（如 `x`）。发键时机按串口标记卡点（`wait_for`），不靠裸睡——菜单每次
  重画都会 `flush_keys`，早到的键会被当陈旧输入吃掉；屏显-only 且无线可断言的
  界面才用 `wait_after` 估，case 里注明。
- 新增串口日志时想想矩阵能不能用上：凡是黑盒要同步的界面切换，都补一行
  `log::` 标记（如 `menu: ready` / `menu: back from {key}` / `boot: N entries` /
  `boot: [k] title <kind>`）。
- `forbid` 是全局红线（panic/死循环/空解码/AP 启动失败），任一出现即 FAIL；
  `expect` 必须在超时前全部在串口出现；`soak_sec` 用于播放类 case（不断言画面，
  只看 N 秒内无红线——QEMU 里断言不了撕裂，只保活）。
- `artifact` 用于落盘类（`hwbench`）：跑完重新挂载镜像，断言文件存在且含关键字。

## 新增 case 规范（和 AGENTS 规则配套）

1. 有新配置项/新路径 → 至少加一个 `matrix.toml` case（config 内联 + expect 串口行）。
2. 有新串口日志 → 老 case 的 `expect` 能用就复用，不硬造新 case。
3. 慢 case（>5min）标 `slow = true`，默认跳过，`--include-slow` 才跑。
4. 多卷场景用 `extra_volumes`（第二盘起挂 hd2、hd3…）：`{path, text|zeros|efi}` 三选一
   组文件；跨盘 chainload 的被起项直接用 `efi = true`（我们的 efi 改名）。
