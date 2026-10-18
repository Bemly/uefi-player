//! EFI chainload（基于 `edera-sprout-eficore`，`boot` bin 不可依赖，自研组装）。
//!
//! 序列照抄上游 `crates/boot/src/actions/chainload.rs`（只读学习，未改上游）：
//! resolve → `ImageLoader::load`（Secure Boot/shim 在里面处理）→
//! `LoadedImage.set_load_options` → `start_image`。
//! P1 范围：只起 PE（BLS `efi`/`uki`）；`linux` + `initrd` 内核直启、devicetree
//! 都不在本阶段（`entries.rs` 把 Linux 项标出来但拒绝启动，如实报错）。

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

    let me = uefi::boot::image_handle();

    // 以自身镜像设备路径为默认根解析：`\EFI\foo.efi` 这类相对路径落到同盘。
    let loaded = uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(me)?;
    let our_path = loaded
        .file_path()
        .ok_or_else(|| NyaStatus::_Debug(String::from("boot: no self image path")))?;
    let resolved = eficore::path::resolve_path(Some(our_path), efi_path).map_err(anyhow_err)?;
    drop(loaded);

    let request = ImageLoadRequest::new(me, ImageSource::ResolvedPath(&resolved));
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
    guard.0 = None;
    uefi::boot::start_image(*image.handle())?;
    Ok(())
}
