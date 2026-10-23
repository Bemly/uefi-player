//! EFI chainload（基于 `edera-sprout-eficore` + `edera-sprout-parsing` 现货组装；
//! `boot` lib 经 `patches/` 叠 `[lib]` 后直调 `boot_counter`，见 `entries.rs`）。
//!
//! 与上游 `crates/boot/src/actions/chainload.rs` 的偏离（QEMU 实测结论）：
//! 上游用 `resolve_path`（DevicePathFromText 文本往返 + `locate_device_path`）
//! 把路径解成 `ResolvedPath`；QEMU 实测该调用触发 OVMF 调试版
//! `MemoryAllocationLib.c:821` ASSERT（分配失败）进 `CpuDeadLoop`，
//! 串口停在 `boot: resolving`，全核 pause 自旋，栈上 ASSERT 字符串为证。
//! 故 P1 全程只用二进制 DevicePath 操作（零文本解析）：
//! 取自身设备句柄的 DevicePath（不能从自身 `LoadedImage::file_path` 截根——
//! fallback 启动时它可能就是裸 `\EFI\...`，无设备前缀，QEMU 实测锤过）+
//! 新 `FilePath` 节点拼出 `ResolvedPath`（`filesystem_handle` 直接用自身设备句柄，
//! 免 `locate_device_path` 一次调用）。
//! 文件经本卷直读，以 `ImageSource::DataBuffer{Some(resolved), bytes}` 交
//! `ImageLoader::load`（`into_owned_data_buffer` 对 DataBuffer 不重读文件，
//! 直接用给定 buffer；`file_path` 照给，子镜像设备上下文完整）。
//! 自检：`locate_device_path` 验证拼出的路径能定位回本卷（串口 `locate ok`），
//! 否则 file_path 给了也白给（曾因此子实例无卷）。
//! 跨盘：目标卷设备句柄拼路径 + 从该卷直读（`volume` 参数，本卷传 `None`）。
//! `LoadedImage.set_load_options`（BLS `options` 透传）与 `UnloadGuard` 照抄上游。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{NyaStatus, Result};

fn anyhow_err(e: anyhow::Error) -> NyaStatus {
    NyaStatus::_Debug(format!("boot: {:#}", e))
}

/// 加载失败不留残留镜像（学上游 `UnloadGuard`：start 前出错即 unload）。
struct UnloadGuard(Option<uefi::Handle>);

impl Drop for UnloadGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            if let Err(e) = uefi::boot::unload_image(handle) {
                log::warn!("boot: unload image failed: {:?}", e);
            }
        }
    }
}

/// 成功不返回（控制权交给新镜像）；被起镜像主动返回时 Ok（调用方重建 Screen 回菜单）。
/// `volume=None` = 本卷（老行为）；`Some(h)` = 跨盘卷（目标设备句柄拼路径 +
/// 从该卷直读；单盘场景全覆盖，跨盘本来就不支持，见 bootloader.md——现已支持）。
pub fn chainload_efi(
    efi_path: &str,
    options: Option<&str>,
    volume: Option<uefi::Handle>,
) -> Result {
    use eficore::loader::source::ImageSource;
    use eficore::loader::{ImageLoadRequest, ImageLoader};

    // 含设备前缀（如 `PciRoot(...)/...`）= 跨盘写法，BLS/自动发现不产这种路径；
    // 真跨盘走 `volume` 参数（见上），这里照旧拒绝。
    if efi_path.contains('(') {
        return Err(NyaStatus::_Debug(format!(
            "boot: cross-device entry unsupported in P1: {}",
            efi_path
        )));
    }
    // BLS 路径是 `/` 分隔（systemd 风格）；UEFI 文件 API 与 FilePath 节点都要 `\`，
    // 不转直接 INVALID_PARAMETER（QEMU 黑盒矩阵实测，见 matrix `boot-chainload`）。
    let efi_path = efi_path.replace('/', "\\");

    let me = uefi::boot::image_handle();

    // 里程碑日志（串口-only）：handoff 路径卡死时靠最后行号定位。
    log::warn!("boot: reading {}", efi_path);
    // 目标卷设备句柄：跨盘用条目的卷，本卷沿用自身 LoadedImage.device()。
    let dev = match volume {
        Some(h) => h,
        None => {
            let loaded =
                uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(me)?;
            let dev = loaded
                .device()
                .ok_or_else(|| NyaStatus::_Debug(String::from("boot: no self image device")))?;
            drop(loaded);
            dev
        }
    };
    let efi_cstr = uefi::data_types::CString16::try_from(efi_path.as_str())
        .map_err(|_| NyaStatus::FromStrWithBufError)?;
    // 二进制拼目标路径（目标卷设备句柄的 DevicePath + 新 FilePath 节点，无文本往返）。
    // 注意不能从自身 LoadedImage::file_path 截根：fallback 启动时它可能就是裸 `\EFI\...`
    //（无设备前缀，QEMU 实测锤过），截出来是空根，locate 必败。
    let resolved = build_resolved_path(dev, &efi_cstr)?;

    // 本卷/跨卷直读（复用 `Fs`，entries 也只枚举本卷，语义一致）。
    let data = crate::fs::Fs::open(volume)?.read_file(&efi_cstr)?;

    // 自检：拼出来的路径必须能 locate 到本卷，否则 file_path 给了也白给。
    // （QEMU 实测定位用，串口看 `locate ok/failed` + hex 对比原路径。）
    {
        fn hexline(tag: &str, b: &[u8]) {
            let mut s = alloc::string::String::from(tag);
            for (i, byte) in b.iter().enumerate() {
                if i >= 160 {
                    s.push_str("...");
                    break;
                }
                s.push_str(&alloc::format!("{:02x}", byte));
            }
            log::warn!("{}", s);
        }
        hexline("boot: rebuilt = ", resolved.full_path.as_bytes());
        let mut probe: &uefi::proto::device_path::DevicePath = &resolved.full_path;
        match uefi::boot::locate_device_path::<uefi::proto::media::fs::SimpleFileSystem>(&mut probe)
        {
            Ok(h) if h == dev => log::warn!("boot: locate ok (own volume)"),
            Ok(_) => log::warn!("boot: locate ok (OTHER volume?)"),
            Err(e) => log::warn!("boot: locate failed: {:?}", e.status()),
        }
    }

    let request = ImageLoadRequest::new(
        me,
        ImageSource::DataBuffer {
            path: Some(&resolved),
            buffer: &data,
        },
    );
    log::warn!("boot: loading image ({} bytes)", data.len());
    let image = ImageLoader::load(request).map_err(anyhow_err)?;
    let mut guard = UnloadGuard(Some(*image.handle()));

    // BLS `options` 行透传（空串按无处理）；pin 住等 start 完成再释放
    // （绑定必须活在函数作用域：LoadedImage 只存裸指针，提前 drop 即悬垂）。
    let mut _opts_keep = None;
    if let Some(opts) = edera_sprout_parsing::empty_is_none(options) {
        let opts = alloc::boxed::Box::pin(
            uefi::CString16::try_from(opts).map_err(|_| NyaStatus::FromStrWithBufError)?,
        );
        if opts.num_bytes() > u32::MAX as usize {
            return Err(NyaStatus::_Debug(String::from("boot: options too large")));
        }
        {
            let mut lip = uefi::boot::open_protocol_exclusive::<
                uefi::proto::loaded_image::LoadedImage,
            >(*image.handle())?;
            unsafe {
                lip.set_load_options(opts.as_ptr() as *const u8, opts.num_bytes() as u32);
            }
        }
        _opts_keep = Some(opts);
    }

    // 即将交权：镜像不再归我们 unload（固件在它退出时处理）。
    log::warn!("boot: starting image");
    guard.0 = None;
    uefi::boot::start_image(*image.handle())?;
    drop(_opts_keep);
    Ok(())
}

/// Linux 直启（P1 全功能对齐新增）：kernel LoadImage + set_load_options +
/// initrd 拼接（`LINUX_EFI_INITRD_MEDIA_GUID` 交接）+ devicetree 安装。
/// 语义与上游 `actions/chainload.rs` 一致（读它学的，自研实现）：
/// - options 为空时填 `placeholder`（Canonical stubble 的非空断言，上游同款规避）。
/// - initrd 多个按序拼接（`parsing::append_initrd` 现货）。
/// - devicetree 装不上只记 warning 照起（Secure Boot 开时直接跳过，上游同款）。
pub fn chainload_linux(
    kernel: &str,
    initrd: &[String],
    options: Option<&str>,
    devicetree: Option<&str>,
    volume: Option<uefi::Handle>,
) -> Result {
    use eficore::loader::source::ImageSource;
    use eficore::loader::{ImageLoadRequest, ImageLoader};

    let me = uefi::boot::image_handle();
    let kernel = kernel.replace('/', "\\");
    log::warn!("boot: reading kernel {}", kernel);

    let dev = match volume {
        Some(h) => h,
        None => {
            let loaded =
                uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(me)?;
            let dev = loaded
                .device()
                .ok_or_else(|| NyaStatus::_Debug(String::from("boot: no self image device")))?;
            drop(loaded);
            dev
        }
    };
    let kernel_cstr = uefi::data_types::CString16::try_from(kernel.as_str())
        .map_err(|_| NyaStatus::FromStrWithBufError)?;
    let resolved = build_resolved_path(dev, &kernel_cstr)?;
    let data = crate::fs::Fs::open(volume)?.read_file(&kernel_cstr)?;

    // 自检：拼出来的路径必须能 locate 回目标卷（同 PE 路）。
    {
        let mut probe: &uefi::proto::device_path::DevicePath = &resolved.full_path;
        match uefi::boot::locate_device_path::<uefi::proto::media::fs::SimpleFileSystem>(&mut probe)
        {
            Ok(h) if h == dev => log::warn!("boot: locate ok (own volume)"),
            Ok(_) => log::warn!("boot: locate ok (OTHER volume?)"),
            Err(e) => log::warn!("boot: locate failed: {:?}", e.status()),
        }
    }

    let request = ImageLoadRequest::new(
        me,
        ImageSource::DataBuffer {
            path: Some(&resolved),
            buffer: &data,
        },
    );
    log::warn!("boot: loading kernel ({} bytes)", data.len());
    let image = ImageLoader::load(request).map_err(anyhow_err)?;
    let mut guard = UnloadGuard(Some(*image.handle()));

    // options 为空填 placeholder（stubble 非空断言，上游同款）。
    let opts = edera_sprout_parsing::empty_is_none(options.map(str::trim)).unwrap_or("placeholder");
    // 绑定活到 start_image 返回（LoadedImage 只存裸指针，块内 drop 即悬垂）。
    let opts = alloc::boxed::Box::pin(
        uefi::CString16::try_from(opts).map_err(|_| NyaStatus::FromStrWithBufError)?,
    );
    if opts.num_bytes() > u32::MAX as usize {
        return Err(NyaStatus::_Debug(String::from("boot: options too large")));
    }
    {
        let mut lip = uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(
            *image.handle(),
        )?;
        unsafe {
            lip.set_load_options(opts.as_ptr() as *const u8, opts.num_bytes() as u32);
        }
    }

    // initrd 逐个读 + 拼接；空集不注册（内核无 initrd 也能起，注册空的反而起不来）。
    let mut initrd_blob: Option<Vec<u8>> = None;
    for path in initrd {
        let path = path.replace('/', "\\");
        if path.trim_matches('\\').is_empty() {
            continue;
        }
        let cstr = uefi::data_types::CString16::try_from(path.as_str())
            .map_err(|_| NyaStatus::FromStrWithBufError)?;
        match crate::fs::Fs::open(volume)?.read_file(&cstr) {
            Ok(content) => {
                log::warn!("boot: initrd {} ({} bytes)", path, content.len());
                match initrd_blob.as_mut() {
                    Some(b) => edera_sprout_parsing::append_initrd(b, &content),
                    None => initrd_blob = Some(content),
                }
            }
            Err(e) => log::warn!("boot: initrd {} unreadable: {:?}, skipped", path, e),
        }
    }
    let mut initrd_handle = None;
    if let Some(content) = initrd_blob.filter(|c| !c.is_empty()) {
        use eficore::media_loader::MediaLoaderHandle;
        use eficore::media_loader::constants::linux::LINUX_EFI_INITRD_MEDIA_GUID;
        match MediaLoaderHandle::register(LINUX_EFI_INITRD_MEDIA_GUID, content.into_boxed_slice()) {
            Ok(h) => initrd_handle = Some(h),
            Err(e) => log::warn!("boot: initrd register failed: {:?}, booting without", e),
        }
    }

    // devicetree：装不上只记 warning（Secure Boot 开时直接跳过，上游同款）。
    let mut dt_handle = None;
    if let Some(path) = edera_sprout_parsing::empty_is_none(devicetree) {
        let path = path.replace('/', "\\");
        match install_devicetree(&path, volume) {
            Ok(h) => dt_handle = h,
            Err(e) => log::warn!("boot: devicetree skipped: {:?}", e),
        }
    }

    log::warn!("boot: starting image");
    guard.0 = None;
    let result = uefi::boot::start_image(*image.handle());
    drop(initrd_handle);
    drop(dt_handle);
    drop(opts);
    result?;
    Ok(())
}

/// devicetree 安装（`eficore::devicetree::DeviceTree::install`），失败/ Secure Boot
/// 开时返回 Err（调用方记 warning 照起，systemd-boot 严格模式除外——我们不做严格模式）。
fn install_devicetree(
    path: &str,
    volume: Option<uefi::Handle>,
) -> Result<Option<eficore::devicetree::DeviceTree>> {
    use eficore::secure::SecureBoot;
    if SecureBoot::enabled().unwrap_or(true) {
        return Err(NyaStatus::_Debug(String::from(
            "devicetree skipped: secure boot",
        )));
    }
    let cstr =
        uefi::data_types::CString16::try_from(path).map_err(|_| NyaStatus::FromStrWithBufError)?;
    let content = crate::fs::Fs::open(volume)?.read_file(&cstr)?;
    eficore::devicetree::DeviceTree::install(&content)
        .map_err(|e| NyaStatus::_Debug(format!("boot: devicetree install failed: {:?}", e)))
        .map(Some)
}

/// 二进制拼 `ResolvedPath`：自身设备句柄的 DevicePath（= 设备根）+
/// 新 `FilePath` 节点。`filesystem_handle` 直接用自身设备句柄。
/// 全程无 DevicePath 文本解析（EDK2 调试版 ASSERT 的触发面，见文件头）。
fn build_resolved_path(
    dev: uefi::Handle,
    efi_cstr: &uefi::data_types::CString16,
) -> Result<eficore::path::ResolvedPath> {
    use uefi::proto::device_path::build::{DevicePathBuilder, media::FilePath as BuildFilePath};
    use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};

    fn is_file_node(node: &uefi::proto::device_path::DevicePathNode) -> bool {
        node.device_type() == DeviceType::MEDIA && node.sub_type() == DeviceSubType::MEDIA_FILE_PATH
    }
    fn build_err() -> NyaStatus {
        NyaStatus::_Debug(String::from("boot: device path build failed"))
    }

    // 自身设备句柄的 DevicePath（协议借用只活到 to_boxed 拷贝完）。
    let dev_path = uefi::boot::open_protocol_exclusive::<uefi::proto::device_path::DevicePath>(dev)
        .map_err(|e| NyaStatus::_Debug(format!("boot: no device path: {:?}", e)))?;
    // 根：前导非文件节点（设备句柄路径本就不该含文件节点，防御性过滤）。
    let mut root_v = Vec::new();
    let root_box = {
        let mut b = DevicePathBuilder::with_vec(&mut root_v);
        for node in dev_path.node_iter() {
            if is_file_node(node) {
                break;
            }
            b = b.push(&node).map_err(|_| build_err())?;
        }
        let root: &DevicePath = b.finalize().map_err(|_| build_err())?;
        root.to_boxed()
    };
    drop(dev_path);
    // 子：纯 FilePath 节点。
    let sub_box = {
        let mut sub_v = Vec::new();
        let sub: &DevicePath = DevicePathBuilder::with_vec(&mut sub_v)
            .push(&BuildFilePath {
                path_name: efi_cstr.as_ref(),
            })
            .map_err(|_| build_err())?
            .finalize()
            .map_err(|_| build_err())?;
        sub.to_boxed()
    };
    // 全：根节点 + FilePath 节点。
    let full_box = {
        let mut full_v = Vec::new();
        let mut b = DevicePathBuilder::with_vec(&mut full_v);
        for node in root_box.node_iter() {
            if is_file_node(node) {
                break;
            }
            b = b.push(&node).map_err(|_| build_err())?;
        }
        let full: &DevicePath = b
            .push(&BuildFilePath {
                path_name: efi_cstr.as_ref(),
            })
            .map_err(|_| build_err())?
            .finalize()
            .map_err(|_| build_err())?;
        full.to_boxed()
    };

    Ok(eficore::path::ResolvedPath {
        root_path: root_box,
        sub_path: sub_box,
        full_path: full_box,
        filesystem_handle: dev,
    })
}
