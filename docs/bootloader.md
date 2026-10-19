# uefi-player 引导（基于 sprout 扩展 → 全功能对齐）

> `third_party/sprout` 只读 submodule，零修改。只取 4 个 lib：
> `edera-sprout-config` / `edera-sprout-eficore` / `edera-sprout-bls` /
> `edera-sprout-parsing`（`boot` 是 bin，不可依赖，读它只学思路，自研实现）。
> 目标：引导能力与 sprout 持平（见 AGENTS「sprout 全功能对齐」），
> UI/配置保持本仓（`player.toml` + config-gui + 我们的菜单）。

## U 盘布局（BLS 标准 + 可选 sprout 覆盖）

```text
\loader\loader.conf          default/timeout（只用 default；@saved 按无默认处理）
\loader\entries\*.conf       BLS 启动项（title/linux/initrd/efi/uki/options/...）
\sprout.toml                 可选；只取 options.default_entry 覆盖 loader.conf
```

## `src/boot/` 四件套

- `entries.rs`：枚举 + 解析 + boot counting 消费。
  `BlsEntry::from_str` / `LoaderConf::parse` / `BootCounter::parse`（sprout 现货）；
  目录枚举 `Fs::list_dir` 自研（`uefi-rs` 无 walker）；默认匹配自研单 `*` 通配。
  无 tries 项跳过并 warning；`arch` 键非 x64 跳过；`uki` 键按 PE 直接起。
  `consume_try`：启动尝试即递减 tries（读旧写新再删旧，FAT 无 rename，
  用既有原语拼；失败只记 warning，不挡启动）。
- `detect.rs`：多卷自动发现（读 sprout `boot/autoconfigure` 学的，自研）。
  `find_handles::<SimpleFileSystem>` 逐卷扫：本卷 BLS 除外（`entries.rs` 已列，
  默认项只看本卷），其它卷 BLS 全列；全卷 UKI（`EFI/Linux/*.efi`，
  读 `.osrel/.cmdline/.uname/.profile` 建条目，一 profile 一项，
  标题/版本/sort-key 走上游 `from_uki_profile_with`，读不到用文件名）/
  Linux 内核对（`\boot` + `\`，前缀配对 + initramfs 拼接，有 BLS/UKI 的卷跳过，
  `parsing` 轮子）/ Windows（`bootmgfw.efi` 存在即链）/ UEFI Shell
  （`shellx64.efi` 三个候选位，systemd-boot 式，上游没有）。
  失败卷只记 warning，不挡其它卷；标题撞车挂 ` (2)` 后缀（本卷占原名）。
  同卷 BLS+UKI 按上游规则排序（sort_key 字典序 + 版本新者在前，跨卷不混排）。
  `strict=true` 时 BLS 只认本卷 + XBOOTLDR 卷（`eficore::xbootldr` 定位）。
- `chainload.rs`：序列照抄上游，路径解析自研——
  BLS 的 `efi` 路径是 `/` 分隔（systemd 风格），入口先转 `\` 再用
  （UEFI 文件 API 与 FilePath 节点都要反斜杠，不转直接 `INVALID_PARAMETER`，
  QEMU 黑盒矩阵 `boot-chainload` 实测）。
  上游用 `resolve_path`（DevicePathFromText 文本往返 + `locate_device_path`）；
  QEMU 实测该调用触发 OVMF 调试版 `MemoryAllocationLib.c:821` ASSERT
  （分配失败）进 `CpuDeadLoop`（串口停在 `boot: resolving`，
  全核 pause 自旋，栈上 ASSERT 字符串为证）。
  故全程只用二进制 DevicePath 操作（零文本解析）：
  取目标卷设备句柄的 DevicePath（不能从自身 `LoadedImage::file_path` 截根——
  fallback 启动时它可能就是裸 `\EFI\...`，QEMU 实测锤过）+
  新 `FilePath` 节点拼出 `ResolvedPath`
  （`filesystem_handle` 直接用目标卷设备句柄，免 `locate_device_path`），
  文件经目标卷直读，以 `ImageSource::DataBuffer{Some(resolved), bytes}` 交
  `ImageLoader::load`（`into_owned_data_buffer` 对 DataBuffer 不重读文件，
  直接用给定 buffer；`file_path` 照给，子镜像设备上下文完整）。
  Linux 直启：同序列 + `set_load_options`（空 options 填 `placeholder`，
  防 Canonical stubble 非空断言，上游同款）+ 多 initrd 拼接
  （`parsing::append_initrd` 现货）经 `LINUX_EFI_INITRD_MEDIA_GUID` 交接 +
  devicetree 可选安装（Secure Boot 开时跳过，上游同款）。
  失败 `UnloadGuard` 卸镜像，不留残留。
  另：QEMU 须用 x64 OVMF（`edk2-x86_64-code.fd`），IA32 固件起 x64 镜像报
  `Unsupported`（Image type X64 can't be started on IA32 UEFI system）。
- `mod.rs`：Exclusive 子菜单（按键 `b` 进，`0-9a-z` 选，`Enter` 跑默认，`q` 回）。
  成功 chainload 不返回；被起镜像返回时重建屏显回子菜单。
  串口可断言：`boot: N entries` + `boot: [k] title <kind>`（矩阵靠它卡按键）。

## 配置（`[boot]`，GUI 有开关页）

`auto` 总开关（关 = 只列本卷 BLS，老行为）+ `windows` / `linux` / `shell` /
`uki` / `macos` 五类开关。本卷 BLS 永远列（不受开关影响）。macOS 原生引导不做
（要 bless/APFS，超出 UEFI 范围；sprout 也没有），只做 OpenCore 链式接力
（`\EFI\OC\OpenCore.efi` 存在即链；不扫通用 `BOOTX64.EFI`，那可能是我们自己）。

## 与路线图的偏离

- P1 原文“与 run_menu 合流（Shared 形态）”未采用：Shared 借屏不释放 GOP，
  handoff 会楔死下个镜像的 GOP 独占打开。故用 Exclusive（drop → 跑 → 重建）。
- 不搬 sprout 的菜单/phase/extractor/driver/SBAT/交互配置语言
  （见 AGENTS；本仓交互层保持自己的）。

## 验收（QEMU 黑盒矩阵，已跑过）

- 单盘 chainload（`boot-chainload`）：locate ok → loading → starting image →
  第二遍 cfg + 菜单；sprout submodule `git status` 干净。
- 跨盘 Windows（`boot-windows-crossvol`）：第二盘 `bootmgfw.efi`（自家 efi 改名）
  被发现并交权，第二遍 cfg + 菜单。
- 跨盘 BLS（`boot-bls-secondvol`）：第二盘 conf + efi，`0` 选中交权。
- Linux 发现（`boot-linux-detect`）：假内核对被列出（标题 + `<linux>` 进串口），不真起。
- Shell 发现 + 交权（`boot-shell`）：`shellx64.efi`（自家 efi）被发现并交权。
- 真机/U 盘实测见 `usb_reports/` 归档。
