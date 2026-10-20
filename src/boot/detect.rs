//! 多卷自动发现（读 sprout `boot/autoconfigure` 学的，自研实现）。
//!
//! 轮子结论（见根 `AGENTS.md`「轮子优先」）：
//! - 内核/initramfs 前缀表 + 配对规则 + `unique_hash` 有现货
//!   （`edera-sprout-parsing`，5 个 lib 之一，直接用，不自研）。
//! - 卷枚举/目录遍历 uefi-rs 无现成 walker（`find_handles` 有，目录树没有），
//!   用本仓 `Fs::{open, list_dir}` 自研（`entries.rs` 同款做法）。
//! - 与上游的偏离（有意，UI/配置保持本仓，见 AGENTS「sprout 全功能对齐」）：
//!   - 不读 sprout.toml/menu 体系，不进 generator/action 概念，直接产
//!     `BootEntry`，进我们自己的子菜单；开关走 `[boot]` + GUI（本仓交互层）。
//!   - `strict` 模式（`[boot] strict` 开关）：BLS 只认本卷 + XBOOTLDR 卷
//!     （systemd 严格模式同语义；其余类别不受影响）。
//!   - UKI 读全套 profile（`.osrel/.cmdline/.uname/.profile`，上游同款
//!     `uki_profiles`），一 profile 一条目，标题/版本/sort-key 走上游同款，
//!     读不到用文件名。
//!   - macOS 原生引导不做（要 bless/APFS，超出 UEFI 范围；sprout 也没有），
//!     只做 OpenCore 链式接力（`\EFI\OC\OpenCore.efi` 存在即链，`[boot] macos`
//!     开关；不扫通用 `BOOTX64.EFI`，那可能是我们自己，会自举死循环）。
//!   - UEFI Shell 上游没有，是 systemd-boot 式的顺手（同 Windows 形状，零代价）。
//!   - 同卷 BLS+UKI 按上游规则排序（`sort_key` 字典序 + 版本新者在前，
//!     跨卷不混排，见 `entries::sort_boot_group`）。

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

/// OpenCore 链式接力（Windows 同款形状：固定路径存在即链）。
/// 轮子结论：无新轮子，复用本仓既有 `Fs::list_dir` 存在性探测（Windows/Shell 同式）；
/// 不扫通用 `BOOTX64.EFI`——那可能是我们自己（fallback 启动），点了会自举死循环。
const OPENCORE_PATH: &str = "\\EFI\\OC\\OpenCore.efi";

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

/// UKI 全套 profile 读出（`.osrel/.cmdline/.uname/.profile`，上游同款
/// `uki_profiles`），一 profile 一条目，标题/版本/sort-key 走上游同款
/// `BlsEntry::from_uki_profile_with`（strict 差异一并继承）。
/// 错架构/坏文件整文件跳过并说明原因；单 profile 无题时回退文件名。
fn uki_entries(
    fs: &mut Fs,
    path: &str,
    file_name: &str,
    volume: Option<Handle>,
    strict: bool,
) -> Result<Vec<BootEntry>, String> {
    use edera_sprout_bls::{PE_MACHINE_X86_64, UKI_SECTIONS, read_pe, uki_profiles};

    struct Reader {
        file: uefi::proto::media::file::RegularFile,
    }
    impl edera_sprout_bls::ReadAt for Reader {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
            self.file
                .set_position(offset)
                .map_err(|e| anyhow::anyhow!("unable to seek: {:?}", e))?;
            let mut done = 0;
            while done < buf.len() {
                let n = self
                    .file
                    .read(&mut buf[done..])
                    .map_err(|e| anyhow::anyhow!("unable to read: {:?}", e))?;
                if n == 0 {
                    anyhow::bail!("unexpected end of file");
                }
                done += n;
            }
            Ok(())
        }
    }

    let cstr = uefi::data_types::CString16::try_from(path).map_err(|_| String::from("bad path"))?;
    let file = fs
        .open_file(&cstr)
        .map_err(|e| format!("unable to open: {:?}", e))?;
    let mut reader = Reader { file };
    let pe = read_pe(&mut reader, &UKI_SECTIONS).map_err(|e| format!("not a PE UKI: {:#}", e))?;
    if pe.machine != PE_MACHINE_X86_64 {
        return Err(format!("wrong architecture: {:#x}", pe.machine));
    }
    let stem = file_name
        .strip_suffix(".efi")
        .or_else(|| file_name.strip_suffix(".EFI"))
        .unwrap_or(file_name);
    let profiles = uki_profiles(&pe.sections);
    if profiles.is_empty() {
        return Err(String::from("no profiles"));
    }
    let mut out = Vec::new();
    for p in &profiles {
        let be = edera_sprout_bls::BlsEntry::from_uki_profile_with(p, path, strict);
        out.push(BootEntry {
            title: be.title.clone().unwrap_or_else(|| String::from(stem)),
            file_name: String::from(file_name),
            kind: BootKind::Efi {
                path: String::from(path),
                options: None,
                volume,
            },
            tries_left: None,
            sort_key: be.sort_key.clone(),
            version: be.version.clone(),
        });
    }
    Ok(out)
}

/// 单卷扫描（`volume=None` 即本卷；BLS 在本卷上跳过——`entries::enumerate`
/// 已经列过本卷 BLS，默认项匹配只看本卷，去重靠调用方标题后缀）。
/// `strict_bls`：严格模式下本卷是否允许出 BLS（只认本卷 + XBOOTLDR 卷）。
/// 返回（条目，警告）。失败只记 warning，不断其它卷。
fn scan_volume(
    fs: &mut Fs,
    volume: Option<Handle>,
    is_own: bool,
    strict: bool,
    strict_bls: bool,
    cfg_windows: bool,
    cfg_linux: bool,
    cfg_shell: bool,
    cfg_uki: bool,
    cfg_macos: bool,
    vol_tag: &str,
) -> (Vec<BootEntry>, Vec<String>) {
    use crate::boot::entries::scan_volume as scan_bls;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut w = |s: String| warnings.push(format!("{}: {}", vol_tag, s));

    // BLS + UKI 同属“BLS 家族”，拼完一起按上游规则排序（见 sort_boot_group）。
    let mut bls_group: Vec<BootEntry> = Vec::new();

    // BLS（本卷跳过，见上；strict 模式下其它卷也只认 XBOOTLDR 卷）。
    if !is_own && (!strict || strict_bls) {
        let (mut es, mut ws) = scan_bls(fs, volume);
        bls_group.append(&mut es);
        for x in ws.drain(..) {
            w(x);
        }
    }
    let bls_here = !bls_group.is_empty();

    // UKI：读全套 profile（`.osrel/.cmdline/.uname/.profile`，上游同款
    // `uki_profiles`），一 profile 一条目（标题 `profile_title`，单 profile
    // 时退化为 `.uname`/文件名）；错架构整文件跳过。
    // BLS 条目复用同一排序组（上游 UKI 本来就是 BLS Type #2 同一队列）。
    let n_uki_before = bls_group.len();
    if cfg_uki {
        for name in dir_files(fs, UKI_DIR) {
            if !name.to_ascii_lowercase().ends_with(".efi") {
                continue;
            }
            match uki_entries(fs, &format!("{}\\{}", UKI_DIR, name), &name, volume, strict) {
                Ok(mut es) => bls_group.append(&mut es),
                Err(why) => w(format!("{}: skipped ({})", name, why)),
            }
        }
    }
    let uki_here = bls_group.len() > n_uki_before;
    crate::boot::entries::sort_boot_group(&mut bls_group);
    entries.append(&mut bls_group);

    // Linux 内核对：\boot + \ 扫前缀，initramfs 按后缀配对（上游同规则）。
    // 该卷已有 BLS/UKI 时跳过（上游同款去重：一个内核只留一种入口）。
    if cfg_linux && !bls_here && !uki_here {
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
                    sort_key: None,
                    version: None,
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
                sort_key: None,
                version: None,
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
                sort_key: None,
                version: None,
            });
        }
    }

    // OpenCore 链式接力：固定路径存在即链（Windows 同款形状；strict 不影响，
    // strict 只约束 BLS 来源，上游同款）。
    if cfg_macos {
        let oc = dir_names(fs, "\\EFI\\OC");
        if oc.contains("opencore.efi") {
            entries.push(BootEntry {
                title: String::from("OpenCore"),
                file_name: String::from("OpenCore.efi"),
                kind: BootKind::Efi {
                    path: String::from(OPENCORE_PATH),
                    options: None,
                    volume,
                },
                tries_left: None,
                sort_key: None,
                version: None,
            });
        }
    }

    (entries, warnings)
}

/// 多卷自动发现：本卷 BLS 由 `entries::enumerate` 负责，这里只做
/// “其它卷 BLS + 全卷 UKI/Linux/Windows/Shell”，调用方拼到列表尾部。
/// 任一卷失败只记 warning，不挡其它卷（U盘热插拔/损坏卷是常态）。
/// `strict`（`[boot] strict`）：BLS 只认本卷 + XBOOTLDR 卷（systemd 严格模式）；
/// 其它类别不受影响（上游同款：strict 只约束 BLS 来源）。
pub fn detect() -> (Vec<BootEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let own = own_device();
    let strict = crate::config::boot_strict();
    // XBOOTLDR 卷（strict 才找；找不到当没有，不挡流程）。
    let xbootldr = if strict { find_xbootldr_handle() } else { None };
    if strict && xbootldr.is_none() {
        warnings.push(String::from("strict: no xbootldr partition found"));
    }
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
            strict,
            Some(*h) == xbootldr,
            crate::config::boot_windows(),
            crate::config::boot_linux(),
            crate::config::boot_shell(),
            crate::config::boot_uki(),
            crate::config::boot_macos(),
            &tag,
        );
        entries.append(&mut es);
        warnings.append(&mut ws);
    }
    (entries, warnings)
}

/// XBOOTLDR 卷句柄（strict 模式的第二个 BLS 来源）。任一步失败返回 None
///（无 GPT/无该分区/固件读不到，都按“没有”处理，不挡流程）。
fn find_xbootldr_handle() -> Option<Handle> {
    use uefi::proto::device_path::DevicePath;
    let me = uefi::boot::image_handle();
    let img =
        uefi::boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(me).ok()?;
    // 路径拷出来再放协议（借用活不过 drop）。
    let img_path: alloc::boxed::Box<DevicePath> = img.file_path()?.to_boxed();
    drop(img);
    let xboot = eficore::xbootldr::find_xbootldr(&img_path).ok().flatten()?;
    let mut probe: &DevicePath = &xboot;
    uefi::boot::locate_device_path::<uefi::proto::media::fs::SimpleFileSystem>(&mut probe).ok()
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
