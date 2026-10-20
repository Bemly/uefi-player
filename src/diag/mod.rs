//! 硬件诊断 / demo 集合（原 `test/` 改名）。
//!
//! 注意：这里没有一个是 `cargo test` 意义上的单元测试，
//! 全是需要在 UEFI 真机/QEMU 下运行的诊断项，所以改名避免混淆。
//! 每个子项统一暴露 `run` 函数，由 [`registry::ITEMS`] 注册进开机菜单。
//! 需要独占 GOP 的特例（如切显示模式的 embedded-graphics 演示）
//! 用 `run() -> Result` 签名，见 [`registry::Entry`]。

pub mod common;
pub mod registry;

pub mod async_exec;
pub mod avc_probe;
pub mod avx_checker;
pub mod fw_collect;
pub mod get_cpu_info;
pub mod get_multi_monitor;
pub mod get_smbios;
pub mod hevc_probe;
pub mod input2_probe;
pub mod mp_service;
pub mod multi_core_draw;
pub mod use_embedded_graphics_gop;

#[cfg(target_arch = "x86_64")]
pub mod gop_bench;
#[cfg(target_arch = "x86_64")]
pub mod gop_simd;
#[cfg(target_arch = "x86_64")]
pub mod hw_bench;
