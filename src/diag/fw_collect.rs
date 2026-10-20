//! 固件信息采集：SMBIOS 关键表解析 + PCI 枚举 + 块设备枚举，落盘 `FWINFOnn.TXT`。
//!
//! 和菜单里其他“测试/演示”项不同，这是真采集：
//! - SMBIOS Type 0/1/2/3/17 逐字段解析（BIOS/整机/主板/机箱/内存条）
//! - PCI Root Bridge 配置空间全扫描（VID:DID + 类码 + 子系统 ID）
//! - BlockIo 全枚举（容量/可移动/只读/分区属性 + MBR/GPT 签名探测）

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::slice;

use dmidecode::{EntryPoint, Structure};
use uefi::Identify;
use uefi::boot::{
    OpenProtocolAttributes, OpenProtocolParams, SearchType, image_handle, locate_handle_buffer,
    open_protocol,
};
use uefi::proto::media::block::BlockIO;
use uefi::proto::pci::PciIoAddress;
use uefi::proto::pci::root_bridge::PciRootBridgeIo;

use crate::diag::common::{find_smbios_address, save_report};
use crate::error::Result;
use crate::graphics::Screen;

// 2.1.1 SMBIOS Structure Table Entry Point => 0x1F
const SMBIOS_BUF_LEN: usize = 0x1F;

macro_rules! tline {
    ($report:expr, $screen:expr, $($arg:tt)*) => {{
        let s = alloc::format!($($arg)*);
        log::info!("{}", s);
        $screen.draw_str(&s);
        $report.push(s);
    }};
}

pub fn run(screen: &mut Screen) -> Result {
    let mut report: Vec<String> = Vec::new();
    tline!(report, screen, "== FW Collect ==");

    // 机器标识行：采回多台后靠它区分。
    if let Some(brand) = raw_cpuid::CpuId::new().get_processor_brand_string() {
        let s = format!("cpu: {}", brand.as_str().trim());
        log::info!("{}", s);
        screen.draw_str(&s);
        report.push(s);
    }

    // AVX 可用性摘要（与菜单 8 的完整 SIMD 压测对应，这里只记结论，
    // 免得三台机器对比时还要去翻 HWBENCH 文件）。
    #[cfg(target_arch = "x86_64")]
    cpu_simd_summary(&mut report, screen);

    tline!(report, screen, "-- smbios --");
    smbios_collect(&mut report, screen);
    tline!(report, screen, "-- pci --");
    pci_collect(&mut report, screen);
    tline!(report, screen, "-- block --");
    block_collect(&mut report, screen);

    let body = report.join("\n") + "\n";
    match save_report("FWINFO", body.as_bytes()) {
        Ok(s) => tline!(report, screen, "{}", s),
        Err(e) => tline!(report, screen, "SAVE FAILED: {:?}", e),
    }

    crate::emit!(screen, "-- collect done --");
    Ok(())
}

// ---------- SMBIOS ----------

/// CPU SIMD 可用性摘要：AVX/AVX2/AVX512F 硬件位 + OSXSAVE + 综合可用判定。
/// 综合判定复用 `avx_checker`（内部已做 OSXSAVE 门控，XCR0 读取不会 #UD）。
/// 读 7 号叶前用最大叶号门控，老 CPU 直接读会回落误报，此时按无硬件处理。
#[cfg(target_arch = "x86_64")]
fn cpu_simd_summary(report: &mut Vec<String>, screen: &mut Screen) {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    fn yn(b: bool) -> &'static str {
        if b { "YES" } else { "no" }
    }

    let c1 = __cpuid(1);
    let avx_hw = c1.ecx & (1 << 28) != 0;
    let (mut avx2_hw, mut avx512f_hw) = (false, false);
    if crate::diag::common::cpuid_max_basic() >= 7 {
        let c7 = __cpuid_count(7, 0);
        avx2_hw = c7.ebx & (1 << 5) != 0;
        avx512f_hw = c7.ebx & (1 << 16) != 0;
    }
    // 与菜单 8 一致：固件没开 XSAVE 就开，再看可用性；报告开机采到的固件原状态。
    crate::diag::common::ensure_xsave_enabled();
    let fw = crate::diag::common::firmware_osxsave();
    let usable = crate::diag::avx_checker::check_avx_support();
    tline!(
        report,
        screen,
        "cpu simd: AVX-HW {} | AVX2-HW {} | AVX512F-HW {} | firmware-OSXSAVE {} | AVX-usable {}",
        yn(avx_hw),
        yn(avx2_hw),
        yn(avx512f_hw),
        match fw {
            Some(true) => "on",
            Some(false) => "off",
            None => "unk",
        },
        if usable { "YES" } else { "NO" }
    );
}

fn smbios_collect(report: &mut Vec<String>, screen: &mut Screen) {
    let Some(address) = find_smbios_address() else {
        tline!(report, screen, "smbios: table not found");
        return;
    };
    let buf = unsafe { slice::from_raw_parts(address as *const u8, SMBIOS_BUF_LEN) };
    let Ok(entry_point) = EntryPoint::search(buf) else {
        tline!(report, screen, "smbios: entry point parse failed");
        return;
    };
    let dmi = unsafe {
        slice::from_raw_parts(
            entry_point.smbios_address() as *const u8,
            entry_point.smbios_len() as usize,
        )
    };

    let (mut n_bios, mut n_sys, mut n_board, mut n_chassis, mut n_mem) = (0, 0, 0, 0, 0);
    for table in entry_point.structures(&dmi).flatten() {
        match table {
            Structure::Bios(b) => {
                n_bios += 1;
                tline!(
                    report,
                    screen,
                    "[BIOS] {} | {} | date {} | rom {}",
                    b.vendor,
                    b.bios_version,
                    b.bios_release_date,
                    bios_rom_size(&b)
                );
            }
            Structure::System(s) => {
                n_sys += 1;
                tline!(
                    report,
                    screen,
                    "[System] {} | {} | ver {} | sn {} | uuid {} | sku {} | family {}",
                    s.manufacturer,
                    s.product,
                    s.version,
                    s.serial,
                    s.uuid
                        .map(|u| format!("{}", u))
                        .unwrap_or(String::from("-")),
                    s.sku.unwrap_or("-"),
                    s.family.unwrap_or("-")
                );
            }
            Structure::BaseBoard(b) => {
                n_board += 1;
                tline!(
                    report,
                    screen,
                    "[Board] {} | {} | ver {} | sn {} | asset {} | type {:?}",
                    b.manufacturer,
                    b.product,
                    b.version,
                    b.serial,
                    b.asset.unwrap_or("-"),
                    b.board_type
                );
            }
            Structure::Enclosure(e) => {
                n_chassis += 1;
                tline!(
                    report,
                    screen,
                    "[Chassis] {} | {:?} | ver {} | sn {} | asset {} | boot {:?}",
                    e.manufacturer,
                    e.enclosure_type,
                    e.version,
                    e.serial_number,
                    e.asset_tag_number,
                    e.boot_up_state
                );
            }
            Structure::MemoryDevice(m) => {
                n_mem += 1;
                tline!(
                    report,
                    screen,
                    "[Mem] {} | {} | {:?} | {} | {}MT/s(cfg {}) | {} | sn {} | {}mV",
                    m.device_locator,
                    mem_size(&m),
                    m.memory_type,
                    m.part_number,
                    m.speed.unwrap_or(0),
                    m.configured_memory_speed.unwrap_or(0),
                    m.manufacturer,
                    m.serial,
                    m.configured_voltage.unwrap_or(0)
                );
            }
            _ => {}
        }
    }
    tline!(
        report,
        screen,
        "smbios: bios={} system={} board={} chassis={} memdev={}",
        n_bios,
        n_sys,
        n_board,
        n_chassis,
        n_mem
    );
}

/// BIOS ROM 体积解码：64K*(n+1)；FFh 表示 16MB+，看 extended（单位 MB）。
fn bios_rom_size(b: &dmidecode::Bios) -> String {
    if b.rom_size.basic == 0xFF {
        match b.rom_size.extended {
            Some(mb) => format!("{}MB+", mb),
            None => String::from("16MB+"),
        }
    } else {
        format!("{}KB", 64 * (b.rom_size.basic as u32 + 1))
    }
}

/// SMBIOS Type 17 Size 字段解码：0=空槽，FFFF/7FFF=看 extended_size(MB)，
/// bit15 置位=KB 单位，其余=MB。
fn mem_size(m: &dmidecode::MemoryDevice) -> String {
    match m.size {
        None => String::from("unknown"),
        Some(0) => String::from("empty"),
        Some(0xFFFF) | Some(0x7FFF) => format!("{}MB", m.extended_size),
        Some(v) if v & 0x8000 != 0 => format!("{}KB", v & 0x7FFF),
        Some(v) => format!("{}MB", v),
    }
}

// ---------- PCI ----------

fn pci_collect(report: &mut Vec<String>, screen: &mut Screen) {
    let bridges = match locate_handle_buffer(SearchType::ByProtocol(&PciRootBridgeIo::GUID)) {
        Ok(h) => h,
        Err(e) => {
            tline!(report, screen, "pci: no root bridge ({:?})", e.status());
            return;
        }
    };
    tline!(report, screen, "pci: {} root bridge(s)", bridges.len());

    let mut total = 0u32;
    for (rbi, h) in bridges.iter().enumerate() {
        // 只读配置空间不需要独占：GetProtocol 只取接口指针。
        // 注意：Exclusive 打开 RootBridge 在 OVMF 上直接 #GP，先绕开。
        let mut rb = match unsafe {
            open_protocol::<PciRootBridgeIo>(
                OpenProtocolParams {
                    handle: *h,
                    agent: image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(p) => p,
            Err(e) => {
                tline!(
                    report,
                    screen,
                    "pci rb{}: open denied ({:?})",
                    rbi,
                    e.status()
                );
                continue;
            }
        };
        for bus in 0..=255u8 {
            for dev in 0..32u8 {
                let f0 = PciIoAddress::new(bus, dev, 0);
                let Ok(vid) = rb.pci().read_one::<u16>(f0.with_register(0x00)) else {
                    continue;
                };
                if vid == 0xFFFF {
                    continue; // 空槽：不存在的设备读回全 1
                }
                // Header Type bit7 = 多功能设备才扫 fun1..8。
                let ht = rb.pci().read_one::<u8>(f0.with_register(0x0E)).unwrap_or(0);
                let funs = if ht & 0x80 != 0 { 8 } else { 1 };
                for fun in 0..funs {
                    dump_pci_fun(&mut rb, report, screen, bus, dev, fun, &mut total);
                }
            }
        }
    }
    tline!(report, screen, "pci: {} function(s) total", total);
}

fn dump_pci_fun(
    rb: &mut PciRootBridgeIo,
    report: &mut Vec<String>,
    screen: &mut Screen,
    bus: u8,
    dev: u8,
    fun: u8,
    total: &mut u32,
) {
    let base = PciIoAddress::new(bus, dev, fun);
    let Ok(vid) = rb.pci().read_one::<u16>(base.with_register(0x00)) else {
        return;
    };
    if vid == 0xFFFF {
        return;
    }
    let did = rb
        .pci()
        .read_one::<u16>(base.with_register(0x02))
        .unwrap_or(0);
    let rev = rb
        .pci()
        .read_one::<u8>(base.with_register(0x08))
        .unwrap_or(0);
    let prog = rb
        .pci()
        .read_one::<u8>(base.with_register(0x09))
        .unwrap_or(0);
    let sub = rb
        .pci()
        .read_one::<u8>(base.with_register(0x0A))
        .unwrap_or(0);
    let class = rb
        .pci()
        .read_one::<u8>(base.with_register(0x0B))
        .unwrap_or(0);
    let svid = rb
        .pci()
        .read_one::<u16>(base.with_register(0x2C))
        .unwrap_or(0);
    let sdid = rb
        .pci()
        .read_one::<u16>(base.with_register(0x2E))
        .unwrap_or(0);
    *total += 1;
    tline!(
        report,
        screen,
        "pci {:02x}:{:02x}.{} {:04x}:{:04x} [{}] subsys {:04x}:{:04x} rev {:02x}",
        bus,
        dev,
        fun,
        vid,
        did,
        class_name(class, sub, prog),
        svid,
        sdid,
        rev
    );
}

fn class_name(class: u8, sub: u8, prog: u8) -> String {
    match (class, sub) {
        (0x01, 0x00) => String::from("SCSI"),
        (0x01, 0x01) => String::from("IDE"),
        (0x01, 0x05) => String::from("ATA"),
        (0x01, 0x06) => String::from("SATA"),
        (0x01, 0x07) => String::from("SAS"),
        (0x01, 0x08) => String::from("NVMe"),
        (0x01, _) => String::from("Storage"),
        (0x02, _) => String::from("Network"),
        (0x03, 0x00) => String::from("VGA"),
        (0x03, _) => String::from("Display"),
        (0x04, 0x03) => String::from("Audio"),
        (0x04, _) => String::from("Multimedia"),
        (0x05, _) => String::from("Memory ctrl"),
        (0x06, 0x00) => String::from("Host bridge"),
        (0x06, 0x01) => String::from("ISA bridge"),
        (0x06, 0x04) => String::from("PCI-PCI bridge"),
        (0x06, _) => String::from("Bridge"),
        (0x07, _) => String::from("Comm"),
        (0x08, _) => String::from("System peripheral"),
        (0x09, _) => String::from("Input"),
        (0x0C, 0x03) => match prog {
            0x00 => String::from("USB UHCI"),
            0x10 => String::from("USB OHCI"),
            0x20 => String::from("USB EHCI"),
            0x30 => String::from("USB XHCI"),
            _ => String::from("USB"),
        },
        (0x0C, _) => String::from("Serial bus"),
        (0x0D, _) => String::from("Wireless"),
        _ => format!("class {:02x}:{:02x}", class, sub),
    }
}

// ---------- Block ----------

fn block_collect(report: &mut Vec<String>, screen: &mut Screen) {
    let handles = match locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID)) {
        Ok(h) => h,
        Err(e) => {
            tline!(report, screen, "block: none ({:?})", e.status());
            return;
        }
    };
    tline!(report, screen, "block: {} handle(s)", handles.len());

    for (i, h) in handles.iter().enumerate() {
        // 同 PCI：只读不独占。尤其不能独占自己启动盘的 BlockIO，
        // 否则后续 FS 写文件冲突（实测 INVALID_PARAMETER）。
        let mut blk = match unsafe {
            open_protocol::<BlockIO>(
                OpenProtocolParams {
                    handle: *h,
                    agent: image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(b) => b,
            Err(_) => {
                tline!(report, screen, "  [{}] open denied", i);
                continue;
            }
        };
        // 先把 media 的标量拷出来：read_blocks 要 &mut blk，media 的借用得先结束。
        let (present, bs, last_block, media_id, is_logical, is_ro, is_removable) = {
            let m = blk.media();
            (
                m.is_media_present(),
                m.block_size(),
                m.last_block(),
                m.media_id(),
                m.is_logical_partition(),
                m.is_read_only(),
                m.is_removable_media(),
            )
        };
        if !present {
            tline!(report, screen, "  [{}] no media", i);
            continue;
        }
        let cap = (last_block as u64 + 1) * bs as u64;
        let sig = read_part_sig(&mut blk, media_id, bs);
        tline!(
            report,
            screen,
            "  [{}] {} {} bs={}{}{} [{}]",
            i,
            if is_logical { "part" } else { "disk" },
            human_bytes(cap),
            bs,
            if is_ro { " ro" } else { "" },
            if is_removable { " removable" } else { "" },
            sig
        );
    }
}

/// 读 LBA0 判分区表签名：GPT / MBR / 裸盘。best-effort，读失败不报错。
/// 注：uefi 0.41 起 `read_blocks` 收 `&mut self`（固件会更新 media 结构）。
fn read_part_sig(blk: &mut BlockIO, media_id: u32, bs: u32) -> &'static str {
    if !(512..=8192).contains(&bs) {
        return "bs?";
    }
    let mut buf = alloc::vec![0u8; bs as usize];
    if blk.read_blocks(media_id, 0, &mut buf).is_err() {
        return "unreadable";
    }
    if buf.starts_with(b"EFI PART") {
        "GPT"
    } else if buf.len() >= 512 && buf[510] == 0x55 && buf[511] == 0xAA {
        "MBR"
    } else {
        "raw"
    }
}

fn human_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1}GiB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1}MiB", b as f64 / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.0}KiB", b as f64 / 1024.0)
    } else {
        format!("{}B", b)
    }
}
