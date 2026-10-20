//! diag 公共 helpers：双写输出、按键等待、SMBIOS 寻址。
//!
//! 背景：本项目历史上混用了 4 种屏显/日志手段
//! （`Screen::draw_str` / `uefi::println!` / `log::*` / embedded-graphics），
//! 各自成因不同。收敛策略：
//! - 图形模式下的用户可见输出统一走 `Screen::draw_str`（`handle_fatal` 用的就是它）；
//! - 串口日志保留（`log::*` 可被 QEMU `-serial` 捕获），所以提供 `emit!` 双写；
//! - `get_multi_monitor.rs` 里那个手写的本地 `println + static mut OFFSET`
//!   是 `Screen::draw_str` 的重复实现，已删除；
//! - 帧内 FPS/OSD（`video.rs::draw_string_opaque`）是显存直写 HUD，不属于文本输出，不动。

use core::sync::atomic::{AtomicU8, Ordering};
use core::time::Duration;

use alloc::format;
use alloc::string::String;
use uefi::boot;
use uefi::data_types::CString16;
use uefi::proto::console::text::Key;
use uefi::system::with_config_table;
use uefi::table::cfg::ConfigTableEntry as cfg;

use crate::error::{NyaStatus, Result};
use crate::fs::Fs;
use crate::graphics::Screen;

/// 双写输出：串口日志（`log`）+ 图形屏显（`Screen::draw_str`），至少一处可见。
///
/// 用法：`emit!(screen, "Total cores: {}", n);`
#[macro_export]
macro_rules! emit {
    ($screen:expr, $($arg:tt)*) => {{
        log::info!($($arg)*);
        $screen.draw_str(&alloc::format!($($arg)*));
    }};
}

/// 把固件给的键值转成 (字符, 原始 u16)。
///
/// 注意：部分固件会把扫描码/高位写进 unicode_char（例如数字键给 0x8038
/// 而不是 0x0038）。所以原始值要带出去，供诊断显示与容错比对（见 key_match）。
fn key_to_char(key: Key) -> Option<(char, u16)> {
    match key {
        Key::Printable(c) => {
            let raw = u16::from(c);
            Some((char::from_u32(raw as u32).unwrap_or('\u{FFFD}'), raw))
        }
        Key::Special(_) => None,
    }
}

/// 排空输入队列里的陈旧按键，避免菜单刚画出来就被之前的按键误触发。
///
/// 传输走 `uefi-input2`（Ex 协议 `reset` 清键码缓存），Ex 缺席回落 legacy ConIn。
/// 双协议各自排队，两边都排空——只清一边会在另一边留下幽灵键。
/// 多键盘机器上仍只取首个句柄（与旧 `with_stdin` 语义一致，见下）。
pub fn flush_keys() {
    let _ = uefi_input2::with_stdin(|input| input.reset(false));
    uefi::system::with_stdin(|input| while matches!(input.read_key(), Ok(Some(_))) {});
}

/// 归一化：小写 + 保留原始码（固件往 unicode 里塞扫描码时靠后者容错，见 `key_match`）。
fn normalize(k: (char, u16)) -> (char, u16) {
    (k.0.to_ascii_lowercase(), k.1)
}

/// Ex 单次非阻塞读（`None` = 无键或 Ex 缺席；调用方回落 legacy）。
fn poll_ex() -> Option<(char, u16)> {
    uefi_input2::with_stdin(|input| {
        Ok(input
            .read_key_stroke_ex()
            .and_then(|kd| key_to_char(kd.key)))
    })
    .ok()
    .flatten()
    .map(normalize)
}

/// legacy ConIn 单次非阻塞读（Ex 缺席时的回落；与旧实现一字不动）。
fn poll_legacy() -> Option<(char, u16)> {
    uefi::system::with_stdin(|input| match input.read_key() {
        Ok(Some(key)) => key_to_char(key).map(normalize),
        _ => None,
    })
}

/// 阻塞等待一个可打印按键，返回 (归一化小写字符, 原始 u16)。
/// Ex 优先、legacy 兜底（见 `poll_key`）；初始排空 + 50ms 轮询与旧语义一致。
pub fn wait_key_ex() -> (char, u16) {
    flush_keys();
    loop {
        if let Some(k) = poll_key() {
            return k;
        }
        boot::stall(Duration::from_millis(50));
    }
}

/// 只要字符的调用方用这个（丢弃原始码）。
pub fn wait_key() -> char {
    wait_key_ex().0
}

/// 非阻塞探一个可打印按键（菜单壁纸 idle 循环用）：有键返回，无键立即 `None`。
/// Ex 优先（`ReadKeyStrokeEx` 本就是非阻塞）、legacy 兜底；归一化规则与旧实现一致
/// （小写 + 原始码），不排空队列、不 stall。修饰键独敲（如纯 Shift）在 Ex 下
/// 落成不可恢复键码，直接过滤（与旧 Special 路一致）。
pub fn poll_key() -> Option<(char, u16)> {
    poll_ex().or_else(poll_legacy)
}

/// 子项跑完后的统一收尾：提示 + 等任意键回菜单。
pub fn pause_return(screen: &mut Screen) {
    screen.draw_str("-- Done. Press any key back to menu --");
    flush_keys();
    wait_key();
}

/// 存报告到启动卷根目录，文件名自动递增（`{prefix}00.TXT`、`{prefix}01.TXT`…），
/// 连测多台机器不互相覆盖。返回屏显用的确认行。
pub fn save_report(prefix: &str, body: &[u8]) -> Result<String> {
    let mut fs = Fs::new()?;
    for i in 0..100u32 {
        let name = format!("{}{:02}.TXT", prefix, i);
        let cstr =
            CString16::try_from(name.as_str()).map_err(|_| NyaStatus::FromStrWithBufError)?;
        if fs.open_file(&cstr).is_err() {
            fs.write_file(&cstr, body)?;
            return Ok(format!("saved: {} ({} bytes)", name, body.len()));
        }
    }
    Err(NyaStatus::_Debug(String::from("too many reports on disk")))
}

/// 在当前 config table 里找 SMBIOS 入口物理地址（优先 SMBIOS3，回落 SMBIOS）。
///
/// 原来 `get_smbios.rs` 两个函数各抄了一份 10 行查找逻辑，抽到这里。
/// 返回 `usize` 形式的物理地址，调用方按需转指针。
pub fn find_smbios_address() -> Option<usize> {
    let mut found: Option<usize> = None;
    with_config_table(|tables| {
        for entry in tables {
            match entry.guid {
                cfg::SMBIOS3_GUID => {
                    found = Some(entry.address as usize);
                    break;
                }
                cfg::SMBIOS_GUID => {
                    found = Some(entry.address as usize);
                }
                _ => {}
            }
        }
    });
    found
}

/// CPUID 最大基本功能号（EAX=0）。读 7 号叶前必须先门控：
/// 老 CPU 没有 7 号叶，直接读会回落返回最大叶内容造成误报。
#[cfg(target_arch = "x86_64")]
pub fn cpuid_max_basic() -> u32 {
    core::arch::x86_64::__cpuid(0).eax
}

/// CPUID 最大扩展功能号（EAX=0x80000000）。读 0x80000001 叶前先门控，同上。
#[cfg(target_arch = "x86_64")]
pub fn cpuid_max_extended() -> u32 {
    core::arch::x86_64::__cpuid(0x8000_0000).eax
}

/// 硬件是否已置 CR4.OSXSAVE（bit18）。CPUID.1:ECX.OSXSAVE 就是这个位的镜像。
#[cfg(target_arch = "x86_64")]
pub fn osxsave_on() -> bool {
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, preserves_flags));
    }
    cr4 & (1 << 18) != 0
}

/// 开机时（任何 enable 之前）采到的**固件原始** CR4.OSXSAVE。
/// 0=未采集 1=off 2=on。各报告引用它，免得被工具自己后来的 enable 污染
/// （此前菜单 8 先开、菜单 9 再读 CPUID 就报了假 YES）。
static FIRMWARE_OSXSAVE: AtomicU8 = AtomicU8::new(0);

/// 在 `main` 初始化后立即调用一次，记录固件原状态。
pub fn capture_firmware_osxsave() {
    #[cfg(target_arch = "x86_64")]
    {
        let v = if osxsave_on() { 2u8 } else { 1u8 };
        FIRMWARE_OSXSAVE.store(v, Ordering::Relaxed);
    }
}

/// 固件原始 OSXSAVE 状态；未采集返回 `None`。
pub fn firmware_osxsave() -> Option<bool> {
    match FIRMWARE_OSXSAVE.load(Ordering::Relaxed) {
        2 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// 确保 XSAVE/AVX 已启用后再跑 AVX 系实测。
///
/// 背景（2026-10-16 QEMU 实锤）：OVMF/EDK2 默认**不开** CR4.OSXSAVE，
/// 于是 CPUID.1:ECX.OSXSAVE 恒 0。按 Intel SDM，CR4.OSXSAVE=0 时执行 VEX
/// 指令会 #UD，所以 `check_avx_support` 的门控是对的——但也意味着 AVX 全矩阵
/// 在 QEMU 里永远 SKIP（HVF/TCG 都一样，因为这是固件策略，不是 CPU 能力）。
///
/// UEFI 应用运行在 CPL0，可以直接写 CR4/XCR0。本函数按需开启：
/// 1. 硬件无 XSAVE（CPUID.1:ECX.26）→ 什么都不做，返回 `false`；
/// 2. CR4.OSXSAVE 未置位 → 置位；
/// 3. XCR0 缺 SSE/AVX(/AVX512) 状态位 → 补齐。
///
/// 返回 `true` 表示**本次由本函数开启**（固件没开、我们自己开），调用方据此
/// 如实报告；已经开着则返回 `false`（幂等，真机上固件多半已开）。
#[cfg(target_arch = "x86_64")]
pub fn ensure_xsave_enabled() -> bool {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    let c1 = __cpuid(1);
    if c1.ecx & (1 << 26) == 0 {
        return false; // 硬件无 XSAVE，开了也是白开
    }
    let has_avx = c1.ecx & (1 << 28) != 0;
    let has_avx512 = cpuid_max_basic() >= 7 && (__cpuid_count(7, 0).ebx & (1 << 16)) != 0;

    let mut changed = false;

    // --- CR4.OSXSAVE (bit18) ---
    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, preserves_flags));
    }
    if cr4 & (1 << 18) == 0 {
        unsafe {
            core::arch::asm!("mov cr4, {0}", in(reg) cr4 | (1u64 << 18), options(nostack));
        }
        changed = true;
    }

    // --- XCR0：置位 OSXSAVE 后才能安全读写 ---
    let mut lo: u32;
    let mut hi: u32;
    unsafe {
        core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi,
            options(nostack, preserves_flags));
    }
    let mut xcr0 = ((hi as u64) << 32) | lo as u64;
    // bit1=SSE(xmm)，bit2=AVX(ymm)，bit5/6/7=AVX512(opmask/zmm)
    let want = (1u64 << 1)
        | if has_avx { 1u64 << 2 } else { 0 }
        | if has_avx512 {
            (1 << 5) | (1 << 6) | (1 << 7)
        } else {
            0
        };
    if xcr0 & want != want {
        xcr0 |= want;
        lo = xcr0 as u32;
        hi = (xcr0 >> 32) as u32;
        unsafe {
            core::arch::asm!("xsetbv", in("ecx") 0u32, in("eax") lo, in("edx") hi,
                options(nostack));
        }
        changed = true;
    }

    changed
}
