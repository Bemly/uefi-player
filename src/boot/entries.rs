//! BLS 启动项枚举：`loader.conf` + `loader/entries/*.conf` + 可选 `sprout.toml`。
//!
//! 轮子结论（见根 `AGENTS.md`「轮子优先」）：
//! - 搜过 `crates.io` BLS 解析：只有 sprout 拆出来的 `edera-sprout-bls`
//!  （`BlsEntry::from_str` + `LoaderConf::parse` + `BootCounter::parse`）是现货，
//!   其余都是 systemd-boot 绑定或整机引导器，不自研解析。
//! - 搜过 sprout 配置：`edera-sprout-config::RootConfiguration` 只描述 sprout
//!   自身菜单（entries/generators/actions/phases 都是它的菜单概念），
//!   这里只借 `options.default_entry` 做默认项覆盖，其余忽略并如实文档化。
//! - 目录枚举无轮子（`uefi-rs` 无现成 walker），`Fs::list_dir` 自研 20 行。
//! - 默认项 glob 自研 10 行（只支持单 `*`，BLS 的 `*.conf`/`arch*` 够用）。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::Handle;
use uefi::cstr16;
use uefi::data_types::CString16;

use crate::error::{NyaStatus, Result};
use crate::fs::Fs;

/// 可 chainload 的项（`efi`/`uki` 都是 PE，直接起），Linux 内核直启超出 P1 范围。
pub enum BootKind {
    Efi {
        path: String,
        options: Option<String>,
        /// 所在卷（`None` = 本卷，老行为；`Some` = 自动发现的跨盘卷）。
        volume: Option<Handle>,
    },
    Linux {
        kernel: String,
        initrd: Vec<String>,
        options: Option<String>,
        devicetree: Option<String>,
        /// 所在卷（`None` = 本卷；`Some` = 自动发现的跨盘卷）。
        volume: Option<Handle>,
    },
}

impl BootKind {
    /// 子菜单/串口用的种类名（与 `BootKind` 变体同义，矩阵断言靠它）。
    pub fn kind_str(&self) -> &'static str {
        match self {
            BootKind::Efi { .. } => "efi",
            BootKind::Linux { .. } => "linux",
        }
    }
}

pub struct BootEntry {
    pub title: String,
    pub file_name: String,
    pub kind: BootKind,
    pub tries_left: Option<u32>,
    /// BLS 排序键（`sort-key` 字段；自动发现项填对应值，无则 None）。
    pub sort_key: Option<String>,
    /// BLS 版本号（`version` 字段；新版排前，见 `sort_boot_group`）。
    pub version: Option<String>,
}

pub struct BootList {
    pub entries: Vec<BootEntry>,
    pub warnings: Vec<String>,
    pub default_index: Option<usize>,
}

/// 本机架构（BLS `architecture` 键过滤用；缺键不断，老文件照进）。
/// 上游按机器字长匹配，这里目标钉死 x86_64，只认 `x64`。
const MACHINE_ARCH: &str = "x64";

/// 单个 BLS conf 解析（`detect.rs` 复用；`volume` 为条目打卷标）。
/// 成功返回条目，失败返回记进 warning 的串（调用方照展示，不 halt）。
/// 偏离上游（有意，见 `detect.rs` 头注释）：`uki` 键按 PE 直接起
/// （不读 `.uname` 区，标题取文件名；要全名请写 conf 标题）。
fn parse_entry(
    name: &str,
    bytes: &[u8],
    volume: Option<Handle>,
) -> core::result::Result<BootEntry, String> {
    use core::str::FromStr;
    let parsed = edera_sprout_bls::BlsEntry::from_str(&String::from_utf8_lossy(bytes))
        .map_err(|e| format!("{}: {:#}", name, e))?;
    let stem = name
        .strip_suffix(".conf")
        .or_else(|| name.strip_suffix(".CONF"))
        .unwrap_or(name);
    let (id, counter) = edera_sprout_bls::BootCounter::parse(stem);
    // boot counting 见光死：没 tries 的不进列表（上游非严格模式会照启，
    // 这里如实跳过并 warning，不断整单）。
    if parsed.is_bad() {
        return Err(format!("{}: no tries left, skipped", name));
    }
    // arch 过滤：缺键放行，错架构跳过（x86_64 固件起不了 arm64 内核）。
    if let Some(arch) = parsed.architecture.as_deref() {
        if arch.to_ascii_lowercase() != MACHINE_ARCH {
            return Err(format!("{}: architecture {:?} skipped", name, arch));
        }
    }
    let title = parsed
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| String::from(id));
    let options = edera_sprout_parsing::empty_is_none(parsed.options.clone());
    let sort_key = parsed.sort_key.clone();
    let version = parsed.version.clone();
    if let Some(efi) = parsed.efi.clone().or_else(|| parsed.uki.clone()) {
        Ok(BootEntry {
            title,
            file_name: String::from(name),
            kind: BootKind::Efi {
                path: efi,
                options,
                volume,
            },
            tries_left: counter.map(|c| c.tries_left),
            sort_key,
            version,
        })
    } else if let Some(kernel) = parsed.linux.clone() {
        Ok(BootEntry {
            title,
            file_name: String::from(name),
            kind: BootKind::Linux {
                kernel,
                initrd: parsed.initrd.clone(),
                options,
                devicetree: parsed.devicetree.clone(),
                volume,
            },
            tries_left: counter.map(|c| c.tries_left),
            sort_key,
            version,
        })
    } else {
        Err(format!("{}: no efi/uki/linux image, skipped", name))
    }
}

/// 条目所在卷（跨盘 `Some`，本卷 `None`）；`mod.rs::volume_of` 同逻辑，
/// 这里为直调上游计数器单独取一次（不经 `Fs`，上游自己开卷）。
fn entry_volume(entry: &BootEntry) -> Option<Handle> {
    match &entry.kind {
        BootKind::Efi { volume, .. } => *volume,
        BootKind::Linux { volume, .. } => *volume,
    }
}

/// BLS 组内排序（上游 `sort_bls` 同规则，减 machine-id 一项）。
/// 有 sort_key 的排前面（字典序），都没有的不动（稳定排序保发现顺序）；
/// 都有则版本号大的在前（`compare_versions` 现货，`10.0` > `2.0` 是数值比）。
/// 上游另按 machine-id 再排一次：同机启动该值恒等，省了；
/// profile 顺序靠稳定排序自然保留（同文件 profile 相邻且等价）。
/// 调用方：同卷 BLS 全量 + UKI 全量拼完调一次（跨卷不混排，卷顺序优先）。
pub fn sort_boot_group(es: &mut [BootEntry]) {
    use core::cmp::Ordering;
    use edera_sprout_bls::compare_versions_optional;
    es.sort_by(|a, b| match (&a.sort_key, &b.sort_key) {
        (Some(x), Some(y)) => x.cmp(y).then_with(|| {
            compare_versions_optional(a.version.as_deref(), b.version.as_deref()).reverse()
        }),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
}

/// 枚举给定卷 `\loader\entries\*.conf`（`volume` 为条目打卷标，
/// `None` = 本卷）。缺目录/无有效项不报错，返回空列表 + warning。
pub fn scan_volume(fs: &mut Fs, volume: Option<Handle>) -> (Vec<BootEntry>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut entries = Vec::new();
    let files = match fs.list_dir(cstr16!("loader\\entries")) {
        Ok(files) => files,
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            warnings.push(String::from("no \\loader\\entries directory"));
            Vec::new()
        }
        Err(e) => {
            warnings.push(format!("entries list failed: {:?}", e));
            Vec::new()
        }
    };
    let mut names: Vec<String> = files
        .into_iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(name, _)| name)
        .filter(|n| n.to_ascii_lowercase().ends_with(".conf"))
        .collect();
    names.sort();
    for name in &names {
        let path = format!("loader\\entries\\{}", name);
        let cstr = match CString16::try_from(path.as_str()) {
            Ok(c) => c,
            Err(_) => {
                warnings.push(format!("{}: bad file name", name));
                continue;
            }
        };
        let bytes = match fs.read_file(&cstr) {
            Ok(b) => b,
            Err(e) => {
                warnings.push(format!("{}: {:?}", name, e));
                continue;
            }
        };
        match parse_entry(name, &bytes, volume) {
            Ok(e) => entries.push(e),
            Err(w) => warnings.push(w),
        }
    }
    (entries, warnings)
}

/// 启动尝试即递减 tries（BLS boot counting）：`name+N-M.conf` 真改名成
/// `name+(N-1)-M.conf`（UEFI `SetInfo` 原子改名，断电不留一对）。
///
/// 轮子结论（见根 `AGENTS.md`「轮子优先」）：直接调上游
/// `edera_sprout_boot::boot_counter::BootCounterTarget::consume`
///（`boot` bin 经 `patches/sprout-boot-lib.patch` 叠 `[lib]` 后可依赖；
/// 上游 `generators/bls.rs` 同款 `counting: true`）。
/// 旧自研是读旧写新再删旧拼 FAT rename，中途断电留一对，已删。
/// 无计数器/设备拿不到不做，失败只记 warning，不挡启动（尝试本身已发生）。
pub fn consume_try(entry: &BootEntry) -> Option<String> {
    use edera_sprout_boot::boot_counter::BootCounterTarget;
    use uefi::fs::PathBuf;
    use uefi::proto::loaded_image::LoadedImage;

    let left = entry.tries_left?;
    let (stem, extension) = match entry.file_name.rfind('.') {
        Some(i) => (
            String::from(&entry.file_name[..i]),
            String::from(&entry.file_name[i..]),
        ),
        None => (entry.file_name.clone(), String::new()),
    };
    let (id, counter) = edera_sprout_bls::BootCounter::parse(&stem);
    let counter = counter?;
    // 跨盘条目用自带卷句柄，本卷条目回查自身 LoadedImage.device()。
    let filesystem = match entry_volume(entry) {
        Some(h) => h,
        None => uefi::boot::open_protocol_exclusive::<LoadedImage>(uefi::boot::image_handle())
            .ok()
            .and_then(|img| img.device())?,
    };
    let mut directory = PathBuf::from(cstr16!("loader"));
    directory.push(cstr16!("entries"));
    let target = BootCounterTarget {
        counter,
        filesystem,
        directory,
        id: String::from(id),
        file_name: String::from(entry.file_name.as_str()),
        extension,
        counting: true,
    };
    match target.consume() {
        Ok(_) => {
            log::warn!(
                "boot: consumed try for {} ({} left)",
                entry.file_name,
                left.saturating_sub(1)
            );
            None
        }
        Err(e) => Some(format!(
            "{}: counter consume failed: {:#}",
            entry.file_name, e
        )),
    }
}

/// 枚举启动卷 `\loader\entries\*.conf` + 默认项匹配（只看本卷 conf）。
/// 缺目录/无有效项不报错，返回空列表 + warning（子菜单如实展示，不 halt 回主菜单）。
pub fn enumerate() -> Result<BootList> {
    let mut fs = Fs::new()?;
    let mut warnings = Vec::new();

    // loader.conf：只要 default（glob 或 @saved；@saved 无状态后端，按无默认处理）。
    let mut default_pattern: Option<String> = None;
    match fs.read_file(cstr16!("loader\\loader.conf")) {
        Ok(bytes) => {
            let conf = edera_sprout_bls::LoaderConf::parse(&String::from_utf8_lossy(&bytes));
            warnings.extend(conf.warnings.iter().map(|w| format!("loader.conf: {}", w)));
            if let Some(p) = conf.default.filter(|p| p != "@saved") {
                default_pattern = Some(p);
            }
        }
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {}
        Err(e) => warnings.push(format!("loader.conf: {:?}", e)),
    }

    // sprout.toml（可选）：只取 options.default_entry 覆盖 loader.conf。
    match fs.read_file(cstr16!("sprout.toml")) {
        Ok(bytes) => {
            match toml::from_str::<edera_sprout_config::RootConfiguration>(
                &String::from_utf8_lossy(&bytes),
            ) {
                Ok(cfg) => {
                    if let Some(d) = cfg.options.default_entry.filter(|d| d != "@saved") {
                        default_pattern = Some(d);
                    }
                }
                Err(e) => warnings.push(format!("sprout.toml: {}", e)),
            }
        }
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {}
        Err(e) => warnings.push(format!("sprout.toml: {:?}", e)),
    }

    // 本卷 BLS（默认项只看本卷；其它卷的由 `detect.rs` 追加，无默认）。
    let (entries, mut scan_warnings) = scan_volume(&mut fs, None);
    warnings.append(&mut scan_warnings);

    let default_index = default_pattern
        .as_deref()
        .and_then(|p| match_default(&entries, p));
    Ok(BootList {
        entries,
        warnings,
        default_index,
    })
}

/// 默认项匹配：文件名（含/不含 `.conf`）、标题精确相等，或单 `*` 通配
///（`*foo` 后缀、`foo*` 前缀、`a*b` 包夹；多个 `*` 不支持，按不匹配）。
fn match_default(entries: &[BootEntry], pattern: &str) -> Option<usize> {
    entries.iter().position(|e| {
        let stem = e
            .file_name
            .strip_suffix(".conf")
            .or_else(|| e.file_name.strip_suffix(".CONF"))
            .unwrap_or(&e.file_name);
        [&e.file_name, stem, &e.title]
            .iter()
            .any(|hay| glob_one(hay, pattern))
    })
}

fn glob_one(hay: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        None => hay == pattern,
        Some((head, tail)) => {
            if tail.contains('*') {
                return false;
            }
            hay.len() >= head.len() + tail.len() && hay.starts_with(head) && hay.ends_with(tail)
        }
    }
}
