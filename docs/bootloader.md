# uefi-player 引导（基于 sprout 扩展）

> `third_party/sprout` 只读 submodule，零修改。只取 4 个 lib：
> `edera-sprout-config` / `edera-sprout-eficore` / `edera-sprout-bls` /
> `edera-sprout-parsing`（`boot` 是 bin，不可依赖，读它只学 chainload 序列）。

## U 盘布局（BLS 标准 + 可选 sprout 覆盖）

```text
\loader\loader.conf          default/timeout（只用 default；@saved 按无默认处理）
\loader\entries\*.conf       BLS 启动项（title/linux/initrd/efi/uki/options/...）
\sprout.toml                 可选；只取 options.default_entry 覆盖 loader.conf
```

## `src/boot/` 三件套

- `entries.rs`：枚举 + 解析。
  `BlsEntry::from_str` / `LoaderConf::parse` / `BootCounter::parse`（sprout 现货）；
  目录枚举 `Fs::list_dir` 自研（`uefi-rs` 无 walker）；默认匹配自研单 `*` 通配。
  无 tries 项跳过并 warning；有 `efi`/`uki` 进 chainload 队列，
  只有 `linux` 的列出但拒绝启动（内核直启超出 P1，见下）。
- `chainload.rs`：序列照抄上游，路径解析自研——
  上游用 `resolve_path`（DevicePathFromText 文本往返 + `locate_device_path`）；
  QEMU 实测该调用触发 OVMF 调试版 `MemoryAllocationLib.c:821` ASSERT
  （分配失败）进 `CpuDeadLoop`（串口停在 `boot: resolving`，
  全核 pause 自旋，栈上 ASSERT 字符串为证）。
  故 P1 全程只用二进制 DevicePath 操作（零文本解析）：
  取自身设备句柄的 DevicePath（不能从自身 `LoadedImage::file_path` 截根——
  fallback 启动时它可能就是裸 `\EFI\...`，QEMU 实测锤过）+
  新 `FilePath` 节点拼出 `ResolvedPath`
  （`filesystem_handle` 直接用自身设备句柄，免 `locate_device_path`），
  文件经本卷直读，以 `ImageSource::DataBuffer{Some(resolved), bytes}` 交
  `ImageLoader::load`（`into_owned_data_buffer` 对 DataBuffer 不重读文件，
  直接用给定 buffer；`file_path` 照给，子镜像设备上下文完整）。
  `LoadedImage.set_load_options`（BLS `options` 透传）→ `start_image`；
  失败 `UnloadGuard` 卸镜像，不留残留。
  代价：跨盘启动项不支持（entries 本来就只枚举本卷，单盘 U 盘场景全覆盖）。
  另：QEMU 须用 x64 OVMF（`edk2-x86_64-code.fd`），IA32 固件起 x64 镜像报
  `Unsupported`（Image type X64 can't be started on IA32 UEFI system）。
- `mod.rs`：Exclusive 子菜单（按键 `b` 进，`0-9a-z` 选，`Enter` 跑默认，`q` 回）。
  成功 chainload 不返回；被起镜像返回时重建屏显回子菜单。

## 与路线图的偏离

- P1 原文“与 run_menu 合流（Shared 形态）”未采用：Shared 借屏不释放 GOP，
  handoff 会楔死下个镜像的 GOP 独占打开。故用 Exclusive（drop → 跑 → 重建）。
- P1 范围：只起 PE（`efi`/`uki`）。`linux` + `initrd` 内核直启、
  devicetree、`loader.conf timeout` 自动倒计时都不做（子菜单无自动项，如实报错/跳过）。

## 验收（QEMU + 真机，待跑）

QEMU 下能列 BLS entry 并 chainload；sprout submodule `git status` 干净。
QEMU 本仓暂无脚本，真机/U 盘实测见 `usb_reports/` 归档。
