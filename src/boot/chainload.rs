//! EFI chainload（基于 `edera-sprout-eficore`，`boot` bin 不可依赖，自研组装）。
//!
//! 与上游 `crates/boot/src/actions/chainload.rs` 的偏离（实测结论）：
//! 上游用 `resolve_path`（DevicePathFromText 文本往返 + `locate_device_path`）
//! 把路径解成 `ImageSource::ResolvedPath`；QEMU 实测里该调用触发 OVMF 调试版
//! `MemoryAllocationLib.c:821` ASSERT（分配失败）进 `CpuDeadLoop`，
//! 串口停在 `boot: resolving`，RIP 全员 pause 自旋（见 `docs/bootloader.md`）。
//! 故 P1 改走同卷直读 + `ImageSource::DataBuffer`（`LoadImageSource::FromBuffer`
//! 的 `file_path` 明确可选，不影响加载）：
//! 打开**自身镜像所在卷**的 SimpleFileSystem（`LoadedImage::device`），
//! BLS `efi` 路径当卷内相对路径读整文件交 `ImageLoader::load`
//! （Secure Boot/shim 判定在里面，和上游一致）。
//! 代价：跨盘启动项不支持（entries 本来就只枚举本卷，如遇含设备前缀的路径
//! 明确报错，不断整单）。`options` 透传与 `UnloadGuard` 照抄上游。

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

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
pub fn chainload_efi(efi_path: &str, options: Option<&str>) -> Result {
    use eficore::loader::source::ImageSource;
    use eficore::loader::{ImageLoadRequest, ImageLoader};

    // 含设备前缀（如 `PciRoot(...)/...`）= 跨盘，P1 明确不支持。
    if efi_path.contains('(') {
        return Err(NyaStatus::_Debug(format!(
            "boot: cross-device entry unsupported in P1: {}",
            efi_path
        )));
    }

    let me = uefi::boot::image_handle();

    // 里程碑日志（串口-only）：handoff 路径卡死时靠最后行号定位。
    log::warn!("boot: reading {}", efi_path);
    let data = read_from_own_volume(me, efi_path)?;

    let request = ImageLoadRequest::new(
        me,
        ImageSource::DataBuffer {
            path: None,
            buffer: &data,
        },
    );
    log::warn!("boot: loading image ({} bytes)", data.len());
    let image = ImageLoader::load(request).map_err(anyhow_err)?;
    let mut guard = UnloadGuard(Some(*image.handle()));

    // BLS `options` 行透传（空串按无处理）；pin 住等 start 完成再释放。
    if let Some(opts) = edera_sprout_parsing::empty_is_none(options) {
        let opts =
            Box::pin(uefi::CString16::try_from(opts).map_err(|_| NyaStatus::FromStrWithBufError)?);
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
    }

    // 即将交权：镜像不再归我们 unload（固件在它退出时处理）。
    log::warn!("boot: starting image");
    guard.0 = None;
    uefi::boot::start_image(*image.handle())?;
    Ok(())
}

/// 从**自身镜像所在卷**读文件（`LoadedImage::device` → SimpleFileSystem）。
/// 复用 `uefi::fs`（和 `Fs::new` 同路数），不经过 device-path 文本解析。
fn read_from_own_volume(me: uefi::Handle, path: &str) -> Result<alloc::vec::Vec<u8>> {
    let loaded = uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(me)?;
    let dev = loaded
        .device()
        .ok_or_else(|| NyaStatus::_Debug(String::from("boot: no self image device")))?;
    drop(loaded);

    let sfs = uefi::boot::open_protocol_exclusive::<uefi::proto::media::fs::SimpleFileSystem>(dev)?;
    let mut fs = uefi::fs::FileSystem::new(sfs);
    let cstr =
        uefi::data_types::CString16::try_from(path).map_err(|_| NyaStatus::FromStrWithBufError)?;
    fs.read(uefi::fs::Path::new(&cstr))
        .map_err(|e| NyaStatus::_Debug(format!("boot: read {} failed: {:?}", path, e)))
}
