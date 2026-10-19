//! 多卷自动发现（读 sprout `boot/autoconfigure` 学的，自研实现）。
//!
//! 轮子结论（见根 `AGENTS.md`「轮子优先」）：
//! - 内核/initramfs 前缀表 + 配对规则 + `unique_hash` 有现货
//!   （`edera-sprout-parsing`，4 个 lib 之一，直接用，不自研）。
//! - 卷枚举/目录遍历 uefi-rs 无现成 walker（`find_handles` 有，目录树没有），
//!   用本仓 `Fs::{open, list_dir}` 自研（`entries.rs` 同款做法）。
//! - 与上游的偏离（有意，UI/配置保持本仓，见 AGENTS「sprout 全功能对齐」）：
//!   - 不读 sprout.toml/menu 体系，不进 generator/action 概念，直接产
//!     `BootEntry`，进我们自己的子菜单；开关走 `[boot]` + GUI（本仓交互层）。
//!   - 不做 strict/XBOOTLDR 分卷（systemd 严格模式），全卷平等扫——
//!     要的就是"插上就有"，少一个漏报比多一个重复重要。
//!   - UKI 不解析 `.uname` 区（省一次全读），标题取文件名；要全名请写 BLS conf。
//!   - macOS 不做（要 bless/APFS，超出 UEFI 范围；sprout 也没有）。
//!   - UEFI Shell 上游没有，是 systemd-boot 式的顺手（同 Windows 形状，零代价）。
//!   - 排序按发现顺序（本卷 BLS → 逐卷 BLS/UKI/Linux/Windows/Shell），
//!     不做上游的 version-sort（屏显菜单稳定可预期最重要）。

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::Handle;
use uefi::proto::loaded_image::LoadedImage;

use crate::boot::entries::{BootEntry, BootKind};
use crate::fs::Fs;

/// Windows 启动管理器（上游同路径，一字不差）。
const WINDOWS_BOOTMGR: &str = "\\EFI\\Microsoft\\Boot\\bootmgfw.efi";

/// UEFI Shell 候选（systemd-boot 式顺手，上游没有；命中即链）。
const SHELL_CANDIDATES: &[&str] = &[
    "\\shellx64.efi",
    "\\EFI\\shellx64.efi",
    "\\EFI\\BOOT\\shellx64.efi",
];

/// 内核扫描目录（上游同款：`\boot` + 根）。
const KERNEL_SCAN_DIRS: &[&str] = &["\\boot", "\\"];

/// UKI 目录（BLS Type #2：统一内核镜像，上游 `bls.rs::detect` 同款位置）。
const UKI_DIR: &str = "\\EFI\\Linux";

/// 自身卷设备句柄（LoadedImage.device；`find_handles` 的比对基准）。
/// 拿不到就当全是外卷（本卷 BLS 照列，见 `entries::enumerate`）。
fn own_device() -> Option<Handle> {
    uefi::boot::open_protocol_exclusive::<LoadedImage>(uefi::boot::image_handle())
        .ok()
        .and_then(|img| img.device())
}

/// 目录直属文件名集合（小写比对用；目录不存在/不可读按空集，跳过不炸）。
fn dir_names(fs: &mut Fs, dir: &str) -> BTreeSet<String> {
    let cstr = match uefi::CString16::try_from(dir) {
        Ok(c) => c,
        Err(_) => return BTreeSet::new(),
    };
    match fs.list_dir(&cstr) {
        Ok(items) => items
            .into_iter()
            .filter(|(_, is_dir)| !is_dir)
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// 目录直属文件（保真大小写，打开文件用）。
fn dir_files(fs: &mut Fs, dir: &str) -> Vec<String> {
    let cstr = match uefi::CString16::try_from(dir) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match fs.list_dir(&cstr) {
        Ok(items) => items
            .into_iter()
            .filter(|(_, is_dir)| !is_dir)
            .map(|(name, _)| name)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 单卷扫描（`volume=None` 即本卷；BLS 在本卷上跳过——`entries::enumerate`
/// 已经列过本卷 BLS，默认项匹配只看本卷，去重靠调用方标题后缀）。
/// 返回（条目，警告）。失败只记 warning，不断其它卷。
fn scan_volume(
    fs: &mut Fs,
    volume: Option<Handle>,
    is_own: bool,
    cfg_windows: bool,
    cfg_linux: bool,
    cfg_shell: bool,
    cfg_uki: bool,
    vol_tag: &str,
) -> (Vec<BootEntry>, Vec<String>) {
    use crate::boot::entries::scan_volume as scan_bls;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut w = |s: String| warnings.push(format!("{}: {}", vol_tag, s));

    // BLS（本卷跳过，见上）。
    if !is_own {
        let (mut es, mut ws) = scan_bls(fs, volume);
        entries.append(&mut es);
        for x in ws.drain(..) {
            w(x);
        }
    }

    // UKI：EFI/Linux/*.efi 全是可直接起的 PE（标题取文件名，不读 .uname 区）。
    if cfg_uki {
        for name in dir_files(fs, UKI_DIR) {
            if !name.to_ascii_lowercase().ends_with(".efi") {
                continue;
            }
            let stem = name
                .strip_suffix(".efi")
                .or_else(|| name.strip_suffix(".EFI"))
                .unwrap_or(&name);
            entries.push(BootEntry {
                title: String::from(stem),
                file_name: String::from(name.as_str()),
                kind: BootKind::Efi {
                    path: format!("{}\\{}", UKI_DIR, name),
                    options: None,
                    volume,
                },
                tries_left: None,
            });
        }
    }

    // Linux 内核对：\boot + \ 扫前缀，initramfs 按后缀配对（上游同规则）。
    if cfg_linux {
        use edera_sprout_parsing::{
            LINUX_INITRAMFS_PREFIXES, LINUX_KERNEL_PREFIXES, initramfs_candidates,
            match_kernel_prefix,
        };
        for dir in KERNEL_SCAN_DIRS {
            let files = dir_files(fs, dir);
            let lower: BTreeSet<String> = files.iter().map(|n| n.to_ascii_lowercase()).collect();
            for name in &files {
                let low = name.to_ascii_lowercase();
                let Some(prefix) = match_kernel_prefix(&low, LINUX_KERNEL_PREFIXES) else {
                    continue;
                };
                let suffix = &low[prefix.len()..];
                let mut initrd = None;
                for cand in initramfs_candidates(suffix, LINUX_INITRAMFS_PREFIXES) {
                    if lower.contains(&cand) {
                        // 按卷上实际大小写还原（FAT 打开一般不敏感，但存真名最稳）。
                        let real = files
                            .iter()
                            .find(|n| n.to_ascii_lowercase() == cand)
                            .cloned()
                            .unwrap_or(cand);
                        let sep = if *dir == "\\" { "" } else { "\\" };
                        initrd = Some(format!("{}{}{}", dir, sep, real));
                        break;
                    }
                }
                let sep = if *dir == "\\" { "" } else { "\\" };
                entries.push(BootEntry {
                    title: format!("Linux {}", name),
                    file_name: String::from(name.as_str()),
                    kind: BootKind::Linux {
                        kernel: format!("{}{}{}", dir, sep, name),
                        initrd: initrd.into_iter().collect(),
                        // 上游 stubble 占位：空 options 会触发 Canonical stub 的
                        // 非空断言，这里同样放 placeholder（见上游 linux.rs 注释）。
                        options: Some(String::from("placeholder")),
                        devicetree: None,
                        volume,
                    },
                    tries_left: None,
                });
            }
        }
    }

    // Windows：bootmgfw 存在即链（上游同路径）。
    if cfg_windows {
        let names = dir_names(fs, "\\EFI\\Microsoft\\Boot");
        if names.contains("bootmgfw.efi") {
            entries.push(BootEntry {
                title: String::from("Windows Boot Manager"),
                file_name: String::from("bootmgfw.efi"),
                kind: BootKind::Efi {
                    path: String::from(WINDOWS_BOOTMGR),
                    options: None,
                    volume,
                },
                tries_left: None,
            });
        }
    }

    // UEFI Shell：候选路径命中即链（systemd-boot 式，上游没有）。
    if cfg_shell {
        let root = dir_names(fs, "\\");
        let efi = dir_names(fs, "\\EFI");
        let boot = dir_names(fs, "\\EFI\\BOOT");
        let hit = |p: &str| match p {
            "\\shellx64.efi" => root.contains("shellx64.efi"),
            "\\EFI\\shellx64.efi" => efi.contains("shellx64.efi"),
            "\\EFI\\BOOT\\shellx64.efi" => boot.contains("shellx64.efi"),
            _ => false,
        };
        if let Some(path) = SHELL_CANDIDATES.iter().find(|p| hit(p)) {
            entries.push(BootEntry {
                title: String::from("UEFI Shell"),
                file_name: String::from("shellx64.efi"),
                kind: BootKind::Efi {
                    path: String::from(*path),
                    options: None,
                    volume,
                },
                tries_left: None,
            });
        }
    }

    (entries, warnings)
}

/// 多卷自动发现：本卷 BLS 由 `entries::enumerate` 负责，这里只做
/// “其它卷 BLS + 全卷 UKI/Linux/Windows/Shell”，调用方拼到列表尾部。
/// 任一卷失败只记 warning，不挡其它卷（U盘热插拔/损坏卷是常态）。
pub fn detect() -> (Vec<BootEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let own = own_device();
    let handles = match uefi::boot::find_handles::<uefi::proto::media::fs::SimpleFileSystem>() {
        Ok(h) => h,
        Err(e) => {
            warnings.push(format!("volume scan failed: {:?}", e.status()));
            return (entries, warnings);
        }
    };
    // 本卷优先（标题去重时本卷占原名，外卷挂后缀，见 `append_unique`）。
    let mut ordered: Vec<Handle> = Vec::new();
    if let Some(o) = own {
        if handles.contains(&o) {
            ordered.push(o);
        }
    }
    for h in handles {
        if Some(h) != own {
            ordered.push(h);
        }
    }
    for (i, h) in ordered.iter().enumerate() {
        let is_own = Some(*h) == own;
        let tag = if is_own {
            String::from("vol=self")
        } else {
            format!("vol{}", i)
        };
        let mut fs = match Fs::open(Some(*h)) {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!("{}: open failed: {:?}", tag, e));
                continue;
            }
        };
        let (mut es, mut ws) = scan_volume(
            &mut fs,
            Some(*h),
            is_own,
            crate::config::boot_windows(),
            crate::config::boot_linux(),
            crate::config::boot_shell(),
            crate::config::boot_uki(),
            &tag,
        );
        entries.append(&mut es);
        warnings.append(&mut ws);
    }
    (entries, warnings)
}

/// 拼到主列表：标题撞车挂 ` (2)` 后缀（本卷 BLS 先占原名，外卷/自动项让路）。
pub fn append_unique(list: &mut crate::boot::entries::BootList, entries: Vec<BootEntry>) {
    for mut e in entries {
        if list.entries.iter().any(|x| x.title == e.title) {
            let mut n = 2;
            loop {
                let cand = format!("{} ({})", e.title, n);
                if !list.entries.iter().any(|x| x.title == cand) {
                    e.title = cand;
                    break;
                }
                n += 1;
            }
        }
        list.entries.push(e);
    }
}
