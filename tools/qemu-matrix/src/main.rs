//! QEMU 黑盒矩阵 runner（测试代码 100% Rust，见 AGENTS）。
//!
//! 每 case 独立镜像 + 独立 QEMU 进程：塞 `player.toml` → 抓串口 → 发按键 →
//! 断言。串口匹配是字面包含（非正则），通配只支持 `*`（够 `HWBENCH*.TXT` 用）。
//! 系统工具（qemu/OVMF/hdiutil/mtools）只当子进程调，不写测试逻辑。

pub mod args;
pub mod image;
pub mod model;
pub mod qemu;

pub(crate) use self::args::{case_hit, changed_files, force_full_reason, parse_args};
pub(crate) use self::model::{Case, Matrix};
pub(crate) use self::qemu::{build_efi, resolve_ovmf, run_case};

/// 编译期钉住仓库根：runner 从哪跑都找到 matrix.toml。
pub(crate) const TOOL_DIR: &str = env!("CARGO_MANIFEST_DIR");
pub(crate) const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// 加载黑盒矩阵：优先读 `matrix/` 目录（按文件名排序合并，`00-global.toml` 定全局），
/// 无目录时回退单文件 `matrix.toml`（过渡期兼容）。
fn load_matrix() -> Matrix {
    use crate::model::Global;
    let dir = format!("{TOOL_DIR}/matrix");
    if std::path::Path::new(&dir).is_dir() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|x| x.path()))
                    .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        if files.is_empty() {
            eprintln!("no matrix/*.toml in {dir}");
            std::process::exit(2);
        }
        let mut merged = Matrix {
            global: Global::default(),
            case: Vec::new(),
        };
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap_or_else(|e| {
                eprintln!("read {}: {e}", f.display());
                std::process::exit(2);
            });
            let m: Matrix = toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("parse {}: {e}", f.display());
                std::process::exit(2);
            });
            // 全局取首个 Some（`00-global.toml` 排首，定全局；分组文件缺省不覆盖）。
            if merged.global.smp.is_none() {
                merged.global.smp = m.global.smp;
            }
            if merged.global.mem_mb.is_none() {
                merged.global.mem_mb = m.global.mem_mb;
            }
            if merged.global.ovmf.is_none() {
                merged.global.ovmf = m.global.ovmf;
            }
            if merged.global.cpu.is_none() {
                merged.global.cpu = m.global.cpu;
            }
            if merged.global.timeout_sec.is_none() {
                merged.global.timeout_sec = m.global.timeout_sec;
            }
            if merged.global.settle_sec.is_none() {
                merged.global.settle_sec = m.global.settle_sec;
            }
            if merged.global.forbid.is_empty() {
                merged.global.forbid = m.global.forbid;
            }
            merged.case.extend(m.case);
        }
        return merged;
    }
    let text = std::fs::read_to_string(format!("{TOOL_DIR}/matrix.toml")).unwrap_or_else(|e| {
        eprintln!("read matrix.toml: {e}");
        std::process::exit(2);
    });
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("parse matrix.toml: {e}");
        std::process::exit(2);
    })
}

fn main() {
    let args = parse_args();
    let matrix = load_matrix();
    let mut cases: Vec<&Case> = matrix.case.iter().collect();
    if let Some(ref name) = args.case {
        if args.changed {
            eprintln!("[changed] --case wins, --changed ignored");
        }
        cases.retain(|c| &c.name == name);
        if cases.is_empty() {
            eprintln!("no such case: {name}");
            std::process::exit(2);
        }
    } else if args.changed {
        match changed_files() {
            None => eprintln!("[changed] git unavailable, running full matrix"),
            Some(changed) => {
                if changed.is_empty() {
                    println!("[changed] tree clean, nothing to run");
                    return;
                }
                if let Some(reason) = force_full_reason(&changed) {
                    println!("[changed] {reason}; running full matrix");
                } else {
                    println!("[changed] {} file(s): {}", changed.len(), changed.join(", "));
                    let total = cases.len();
                    cases.retain(|c| case_hit(&c.covers, &changed));
                    println!("[changed] selected {total}->{len} cases", len = cases.len());
                    if cases.is_empty() {
                        println!("[changed] no case covers these files, nothing to run");
                        return;
                    }
                }
            }
        }
        if !args.include_slow {
            cases.retain(|c| !c.slow);
        }
    } else if !args.include_slow {
        cases.retain(|c| !c.slow);
    }
    if args.list {
        for c in &cases {
            let slow = if c.slow { " (slow)" } else { "" };
            println!("  {}{} covers: [{}]", c.name, slow, c.covers.join(", "));
        }
        return;
    }

    let efi = match build_efi(!args.no_build) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if !std::path::Path::new(&efi).exists() {
        eprintln!("efi missing: {efi}");
        std::process::exit(1);
    }
    let debug_default = "/usr/local/share/qemu/edk2-x86_64-code.fd";
    // 固件档：--ovmf-set both = 每个 case 跑两遍（debug 原样 + release 挂 -release 后缀）；
    // release = 全用 RELEASE 跑一遍；缺省单跑：命令行 --ovmf > case.ovmf > 全局 ovmf > 内建 debug。
    let set_mode = args.ovmf_set.as_deref().unwrap_or("debug");
    if !["debug", "release", "both"].contains(&set_mode) {
        eprintln!("unknown --ovmf-set {set_mode:?} (want debug|release|both)");
        std::process::exit(2);
    }

    let mut results = Vec::new();
    for c in cases {
        let passes: Vec<(String, String)> = match set_mode {
            "both" => vec![
                (resolve_ovmf(Some("debug"), debug_default), String::new()),
                (
                    resolve_ovmf(Some("release"), debug_default),
                    "-release".to_string(),
                ),
            ],
            "release" => vec![(resolve_ovmf(Some("release"), debug_default), String::new())],
            _ => vec![(
                resolve_ovmf(
                    args.ovmf
                        .as_deref()
                        .or(c.ovmf.as_deref())
                        .or(matrix.global.ovmf.as_deref()),
                    debug_default,
                ),
                String::new(),
            )],
        };
        for (ovmf, suffix) in &passes {
            let cpu = args
                .cpu
                .as_deref()
                .or(c.cpu.as_deref())
                .or(matrix.global.cpu.as_deref())
                .unwrap_or("");
            let (ok, detail) = run_case(c, &matrix.global, &efi, args.keep, ovmf, cpu, suffix);
            results.push((format!("{}{}", c.name, suffix), ok, detail));
        }
    }

    println!("\n==== matrix ====");
    let mut allok = true;
    for (name, ok, detail) in &results {
        if *ok {
            println!("  [PASS] {name}");
        } else {
            println!("  [FAIL] {name} -- {detail}");
        }
        allok = allok && *ok;
    }
    if !allok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod selection_tests {
    use crate::args::{case_hit, force_full_reason, prefixes_hit};

    fn chg(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn star_and_empty() {
        assert!(prefixes_hit(["*"], &chg(&["anything.rs"])));
        assert!(!prefixes_hit([], &chg(&["src/boot/x.rs"])));
        assert!(!prefixes_hit(["src/boot/"], &chg(&[])));
    }

    #[test]
    fn file_and_dir_prefixes() {
        assert!(prefixes_hit(["src/video.rs"], &chg(&["src/video.rs"])));
        assert!(prefixes_hit(["src/video/"], &chg(&["src/video/exec.rs"])));
        // 文件与目录是两个条目："src/video" 盖不住 "src/video.rs"，
        // 也不能误伤 "src/video2/"（边界走 "/" 分隔）。
        assert!(!prefixes_hit(["src/video"], &chg(&["src/video.rs"])));
        assert!(!prefixes_hit(["src/boot/"], &chg(&["src/boots.rs"])));
    }

    #[test]
    fn core_implicit() {
        // covers 为空的纯标定类 case：核心文件动了要跑，非核心不动不跑。
        assert!(case_hit(&[], &chg(&["src/config/mod.rs"])));
        assert!(case_hit(&[], &chg(&["src/config/types.rs"])));
        assert!(!case_hit(&[], &chg(&["src/video/exec.rs"])));
        assert!(case_hit(&chg(&["src/video/"]), &chg(&["src/video/exec.rs"])));
    }

    #[test]
    fn meta_forces_full() {
        assert!(force_full_reason(&chg(&["tools/qemu-matrix/src/main.rs"])).is_some());
        assert!(force_full_reason(&chg(&["Cargo.toml"])).is_some());
        assert!(force_full_reason(&chg(&["third_party/sprout"])).is_some());
        assert!(force_full_reason(&chg(&["src/boot/chainload.rs"])).is_none());
        assert!(force_full_reason(&chg(&["tools/config-gui/src/app.rs"])).is_none());
    }
}
