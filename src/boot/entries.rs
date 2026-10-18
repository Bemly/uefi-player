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
use uefi::cstr16;
use uefi::data_types::CString16;

use crate::error::{NyaStatus, Result};
use crate::fs::Fs;

/// 可 chainload 的项（`efi`/`uki` 都是 PE，直接起），Linux 内核直启超出 P1 范围。
pub enum BootKind {
    Efi {
        path: String,
        options: Option<String>,
    },
    Linux {
        kernel: String,
    },
}

pub struct BootEntry {
    pub title: String,
    pub file_name: String,
    pub kind: BootKind,
    pub tries_left: Option<u32>,
}

pub struct BootList {
    pub entries: Vec<BootEntry>,
    pub warnings: Vec<String>,
    pub default_index: Option<usize>,
}

/// 枚举启动卷 `\loader\entries\*.conf`。缺目录/无有效项不报错，
/// 返回空列表 + warning（子菜单如实展示，不 halt 回主菜单）。
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

    let mut entries = Vec::new();
    let files = match fs.list_dir(cstr16!("loader\\entries")) {
        Ok(files) => files,
        Err(NyaStatus::Uefi(err)) if err.status() == uefi::Status::NOT_FOUND => {
            warnings.push(String::from("no \\loader\\entries directory"));
            Vec::new()
        }
        Err(e) => return Err(e),
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
        let parsed = match String::from_utf8_lossy(&bytes).parse::<edera_sprout_bls::BlsEntry>() {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("{}: {:#}", name, e));
                continue;
            }
        };
        let stem = name
            .strip_suffix(".conf")
            .or_else(|| name.strip_suffix(".CONF"))
            .unwrap_or(name);
        let (id, counter) = edera_sprout_bls::BootCounter::parse(stem);
        // boot counting 见光死：没 tries 的不进列表（sprout 非严格模式会照启，
        // 这里如实跳过并 warning，不断整单）。
        if parsed.is_bad() {
            warnings.push(format!("{}: no tries left, skipped", name));
            continue;
        }
        let title = parsed
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| String::from(id));
        let options = edera_sprout_parsing::empty_is_none(parsed.options.clone());
        if let Some(efi) = parsed.efi.clone().or_else(|| parsed.uki.clone()) {
            entries.push(BootEntry {
                title,
                file_name: String::from(name),
                kind: BootKind::Efi { path: efi, options },
                tries_left: counter.map(|c| c.tries_left),
            });
        } else if let Some(kernel) = parsed.linux.clone() {
            entries.push(BootEntry {
                title,
                file_name: String::from(name),
                kind: BootKind::Linux { kernel },
                tries_left: counter.map(|c| c.tries_left),
            });
        } else {
            warnings.push(format!("{}: no efi/uki/linux image, skipped", name));
        }
    }

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
