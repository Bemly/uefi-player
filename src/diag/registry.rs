//! 开机菜单注册表：每个诊断项占一项，按键分发。
//!
//! - [`Entry::Shared`]：demo 借用菜单的 `Screen`（GOP 仍由 `Screen` 独占）。
//!   这是默认形态；此前各 demo 自己 `open_protocol_exclusive::<GraphicsOutput>`
//!   的写法在菜单化后会与 `Screen` 冲突，已全部改为借用。
//! - [`Entry::Exclusive`]：demo 需要自己独占 GOP（如强制切显示模式）。
//!   菜单会在运行前 `drop(Screen)` 释放独占，跑完重建。目前只有
//!   embedded-graphics 中文演示是这种形态。

use crate::boot;
use crate::diag::{
    avx_checker, fw_collect, get_cpu_info, get_multi_monitor, get_smbios, mp_service,
    multi_core_draw, use_embedded_graphics_gop,
};
#[cfg(target_arch = "x86_64")]
use crate::diag::hw_bench;
use crate::error::Result;
use crate::graphics::Screen;
use crate::video::video_run;

pub enum Entry {
    /// `fn(&mut Screen) -> Result`：共享菜单的 Screen。
    Shared(fn(&mut Screen) -> Result),
    /// `fn() -> Result`：独占 GOP，菜单负责 drop/重建 Screen。
    Exclusive(fn() -> Result),
}

pub struct Item {
    pub key: char,
    pub name: &'static str,
    pub desc: &'static str,
    pub run: Entry,
    /// 出错时是否走致命屏显路径（`handle_fatal`，清屏 + stall + panic）。
    /// 仅生产路径（播放视频）为 true，保持重构前 main 的语义；
    /// 诊断项一律可返回式报错，回菜单继续玩。
    pub fatal_on_error: bool,
}

pub const ITEMS: &[Item] = &[
    Item {
        key: '0',
        name: "Play video",
        desc: "1080p MP parallel playback",
        run: Entry::Shared(video_run),
        fatal_on_error: true,
    },
    Item {
        key: '1',
        name: "CPU info",
        desc: "MP + SMBIOS + CPUID",
        run: Entry::Shared(get_cpu_info::run),
        fatal_on_error: false,
    },
    Item {
        key: '2',
        name: "SMBIOS tables",
        desc: "full DMI dump",
        run: Entry::Shared(get_smbios::run),
        fatal_on_error: false,
    },
    Item {
        key: '3',
        name: "Display modes",
        desc: "GOP modes + reopen probe",
        run: Entry::Shared(get_multi_monitor::run),
        fatal_on_error: false,
    },
    Item {
        key: '4',
        name: "MP self-test",
        desc: "startup_all_aps counter",
        run: Entry::Shared(mp_service::run),
        fatal_on_error: false,
    },
    Item {
        key: '5',
        name: "Multi-core fill",
        desc: "parallel framebuffer paint",
        run: Entry::Shared(multi_core_draw::run),
        fatal_on_error: false,
    },
    Item {
        key: '6',
        name: "CN font demo",
        desc: "embedded-graphics 800x600",
        run: Entry::Exclusive(use_embedded_graphics_gop::run),
        fatal_on_error: false,
    },
    Item {
        key: '7',
        name: "AVX check",
        desc: "CPUID + XCR0 probe",
        run: Entry::Shared(avx_checker::run),
        fatal_on_error: false,
    },
    #[cfg(target_arch = "x86_64")]
    Item {
        key: '8',
        name: "HW bench",
        desc: "full-ISA SIMD+GOP+compute, save USB",
        run: Entry::Shared(hw_bench::run),
        fatal_on_error: false,
    },
    Item {
        key: '9',
        name: "FW collect",
        desc: "SMBIOS+PCI+disks, save USB",
        run: Entry::Shared(fw_collect::run),
        fatal_on_error: false,
    },
    Item {
        key: 'b',
        name: "Boot entries",
        desc: "BLS enumerate + chainload",
        run: Entry::Exclusive(boot::run),
        fatal_on_error: false,
    },
];
