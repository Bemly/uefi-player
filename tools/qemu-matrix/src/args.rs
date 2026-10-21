//! `qemu-matrix::args`：命令行解析 + `--changed` 选择逻辑（`CORE` 隐式核心）。
//! 手写解析（不引 clap），选择依据见 `matrix.toml` 头注释。拆分仅为 ≤1000 行。

use super::image::run;
use super::ROOT;

#[derive(Debug, Default)]
pub(crate) struct Args {
    pub(crate) list: bool,
    pub(crate) case: Option<String>,
    pub(crate) include_slow: bool,
    pub(crate) keep: bool,
    pub(crate) no_build: bool,
    /// 只跑改动波及的 case（迭代加速；提交门禁仍是全矩阵）。
    /// 改动 = 工作区未提交（含 untracked）∪ 分支相对 origin/master 的已提交。
    pub(crate) changed: bool,
    pub(crate) ovmf: Option<String>,
    /// 强制本轮全用指定 CPU（模型名或 "host"）；单 case `cpu` 与全局 `cpu` 照认，
    /// 优先级：命令行 > 单 case > 全局 > 缺省（不用 -cpu，即 qemu64 基线）。
    pub(crate) cpu: Option<String>,
    /// 全局固件档：debug（默认，沿用全局/单 case 设置）、release（全用 RELEASE 跑一遍）、
    /// both（每个 case 跑两遍，第二遍挂 `-release` 后缀；与 `--ovmf` 同给时以它为准）。
    pub(crate) ovmf_set: Option<String>,
}

pub(crate) fn parse_args() -> Args {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--list" => a.list = true,
            "--changed" => a.changed = true,
            "--include-slow" => a.include_slow = true,
            "--keep" => a.keep = true,
            "--no-build" => a.no_build = true,
            "--case" => a.case = it.next(),
            "--ovmf" => a.ovmf = it.next(),
            "--cpu" => a.cpu = it.next(),
            "--ovmf-set" => a.ovmf_set = it.next(),
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

/// `--changed` 隐式核心：这些文件动了等于全矩阵（所有 case 都走开机→解析→屏显）。
/// 写进代码而不是逐 case 重复 8 遍，口径见 README 与 matrix.toml 头注释。
pub(crate) const CORE: &[&str] = &[
    "src/main.rs",
    "src/menu.rs",
    "src/idle.rs",
    "src/graphics",
    "src/diag/common.rs",
    "src/config/",
    "src/fs.rs",
    "src/error.rs",
];

/// 前缀命中：改动文件等于该前缀，或落在其目录下（`"src/boot/"` 盖住
/// `src/boot/chainload.rs`；文件前缀如 `"src/video.rs"` 走等于分支）。
pub(crate) fn prefixes_hit<'a>(covers: impl IntoIterator<Item = &'a str>, changed: &[String]) -> bool {
    for cov in covers {
        if cov == "*" {
            return true;
        }
        // 尾斜杠容忍：`"src/boot/"` 与 `"src/boot"` 同义；边界走 "/" 分隔，
        // `"src/video"` 盖不住 `"src/video.rs"`，也误伤不了 `"src/video2/"`。
        let c = cov.strip_suffix('/').unwrap_or(cov);
        for f in changed {
            if f == cov || f == c || f.starts_with(&format!("{c}/")) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn case_hit(covers: &[String], changed: &[String]) -> bool {
    prefixes_hit(covers.iter().map(String::as_str), changed)
        || prefixes_hit(CORE.iter().copied(), changed)
}

/// 收集改动文件（仓库根相对路径）：工作区未提交（含 untracked `??` 与改名后者）
/// ∪ 分支相对 origin/master 的已提交。git 不可用返回 `None`（调用方全跑）。
/// `third_party/` 工作区恒脏（patches 叠加态）只在 status 侧忽略；
/// 分支 diff 侧保留（pin 变更能被看见，见下 force_full_reason）。
pub(crate) fn changed_files() -> Option<Vec<String>> {
    fn push(out: &mut Vec<String>, raw: &str, filter_third_party: bool) {
        let mut p = raw.trim();
        if let Some(i) = p.find(" -> ") {
            p = &p[i + 4..];
        }
        let p = p.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(p);
        if p.is_empty() {
            return;
        }
        if filter_third_party && p.starts_with("third_party/") {
            return;
        }
        if !out.iter().any(|e| e == p) {
            out.push(p.to_string());
        }
    }
    let status = run("git", &["-C", ROOT, "status", "--porcelain"]).ok()?;
    let mut out = Vec::new();
    for line in status.lines() {
        if line.len() < 4 {
            continue;
        }
        push(&mut out, &line[3..], true);
    }
    if let Ok(base) = run("git", &["-C", ROOT, "merge-base", "HEAD", "origin/master"]) {
        let base = base.trim();
        if !base.is_empty() {
            if let Ok(diff) = run("git", &["-C", ROOT, "diff", "--name-only", base, "HEAD"]) {
                for line in diff.lines() {
                    push(&mut out, line, false);
                }
            }
        }
    }
    Some(out)
}

/// 这些动了选择依据本身可能已过期，直接全跑（并打印原因）。
pub(crate) fn force_full_reason(changed: &[String]) -> Option<&'static str> {
    for f in changed {
        if f.starts_with("tools/qemu-matrix/")
            || f == "Cargo.toml"
            || f.starts_with(".cargo/")
            || f == "rust-toolchain.toml"
            || f == "third_party/sprout"
        {
            return Some("runner/matrix/manifest/toolchain/sprout-pin changed");
        }
    }
    None
}

/// `*` 通配（只够 `HWBENCH*.TXT` 这类前后缀用）。
pub(crate) fn glob_match(pat: &str, name: &str) -> bool {
    let mut rest = name;
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    if !rest.starts_with(parts[0]) {
        return false;
    }
    rest = &rest[parts[0].len()..];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    rest.ends_with(parts[parts.len() - 1])
}

