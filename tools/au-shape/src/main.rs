//! Annex-B AU 切分形状 host 单测执行器（Agent-B，2026-10-21）。
//!
//! 为什么需要这个 crate：固件主包是 `#![no_std] + #![no_main]` 的 bin，
//! 且依赖 `uefi` 的 `global_allocator` / `panic_handler`
//!（另有 `edera-sprout-boot` 的 `.sbat` 段），host target 上根本链不过
//!（实测 `cargo test -p uefi-player --target <host>` 死于 Mach-O section +
//! 重复 panic handler）。故测试仍写在 `src/video/demux.rs` 内 `#[cfg(test)]`
//!（单源头），本 crate 仅 `#[path]` 原样包含该文件并在 host 上执行单测。
//! 运行：`cargo test --manifest-path tools/au-shape/Cargo.toml
//! --target "$(rustc -vV | sed -n 's/^host: //p')"`
//! 固件门禁不受影响（主包 `build/clippy/fmt -p uefi-player` 照旧）。

extern crate alloc;

/// `src/video/demux.rs` 引用的 `crate::error` 最小同形垫片
/// （只覆盖 demux 实际用的 surface：`_Debug(String)` + `Result` 别名；
/// 固件侧真实现见 `src/error.rs`，行为一致：坏输入记数跳过、全坏报错）。
pub mod error {
    use alloc::string::String;
    use core::fmt::Debug;

    pub type Result<Output = (), ErrData = ()> = core::result::Result<Output, NyaStatus<ErrData>>;

    #[derive(Debug)]
    pub enum NyaStatus<Data: Debug = ()> {
        Uefi(Data),
        _Debug(String),
        _Reserve,
    }
}

#[allow(dead_code)]
#[path = "../../../src/video/demux.rs"]
mod demux;

fn main() {
    // 单测走 harness：本 bin 不直接执行拆包逻辑。
    println!("au-shape: run with `cargo test --manifest-path tools/au-shape/Cargo.toml`");
}
