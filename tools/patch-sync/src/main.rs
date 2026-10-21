//! `patches/*.patch` 重打工具：从脏的 `third_party` submodule 工作树再生 patch。
//!
//! 背景（见 `patches/README.md`）：7 个 submodule 冻结 pin，逻辑改动以 patch
//! 叠加上游 checkout。单箱确实就是一行 `git diff`，但三件事必须专人管：
//! - `third_party/rusty_h265` 一个仓出两个 patch（decoder/accel），得按路径劈开；
//! - `git diff` 默认吞掉新增文件（须先 `git add -N`，否则静默丢失）；
//! - 作用域外的随手改（examples/CI/文档）必须拦下，不能混进 firmware patch。
//! 本工具三件事全包：fail-closed，无静默行为。
//!
//! 用法（仓库根目录跑，host target，见根 AGENTS.md「双构建」）：
//! ```bash
//! cargo run --manifest-path tools/patch-sync/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" -- [--check] [--only <name>]
//! ```
//! - 默认：按改动重写 `patches/*.patch`（`git diff` 口径：`diff --git` 头 + `index` 行，
//!   与现存 GNU-diff 口径语义等价，`git apply` 通吃）。
//! - `--check`：只比对（语义归一后），有漂移即非零退出，不写盘。
//! - `--only <name>`：只处理一个 patch 名（如 `rusty_h265-accel`）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// 一个 patch 任务：submodule 内若干作用域根 → 一个 patch 文件。
struct Job {
    /// patch 名（`patches/<name>.patch`），也是 `--only` 的键。
    name: &'static str,
    /// submodule 目录（仓库根相对路径）。
    sub: &'static str,
    /// 作用域根（submodule 内相对路径）：目录收其下一切，文件精确命中。
    /// 作用域外的脏文件一律报错（fail-closed），想收就改这里。
    scope: &'static [&'static str],
}

const JOBS: &[Job] = &[
    Job { name: "oxideav-theora", sub: "third_party/oxideav-theora", scope: &["src/", "Cargo.toml"] },
    Job { name: "vp9dec", sub: "third_party/vp9dec", scope: &["src/", "Cargo.toml"] },
    Job { name: "rusty_h265", sub: "third_party/rusty_h265", scope: &["crates/rusty_h265/src/", "crates/rusty_h265/Cargo.toml"] },
    Job { name: "rusty_h265-accel", sub: "third_party/rusty_h265", scope: &["crates/rusty_h265-accel/src/", "crates/rusty_h265-accel/Cargo.toml"] },
    Job { name: "rusty_h264-accel", sub: "third_party/rusty_h264", scope: &["crates/rusty_h264-accel/src/", "crates/rusty_h264-accel/Cargo.toml"] },
    Job { name: "filmcraft-mpeg2v", sub: "third_party/filmcraft", scope: &["crates/mpeg2v/src/", "crates/mpeg2v/Cargo.toml"] },
    Job {
        name: "h263-rs",
        sub: "third_party/h263-rs",
        scope: &[
            "h263/src/", "h263/Cargo.toml",
            "yuv/src/", "yuv/Cargo.toml",
            "deblock/src/", "deblock/Cargo.toml",
        ],
    },
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut check = false;
    let mut only: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check = true,
            "--only" => {
                only = Some(args.get(i + 1).unwrap_or_else(|| fail("--only needs a value")).as_str());
                i += 1;
            }
            other => fail(&format!("unknown arg: {other} (see file header)")),
        }
        i += 1;
    }

    let root = PathBuf::from(".");
    for need in ["patches", "third_party", ".gitmodules"] {
        if !root.join(need).exists() {
            fail("run from the repo root (patches/ not found)");
        }
    }

    let jobs: Vec<&Job> = JOBS.iter().filter(|j| only.is_none_or(|o| o == j.name)).collect();
    if jobs.is_empty() {
        fail(&format!("--only: unknown patch name ({} known)", JOBS.iter().map(|j| j.name).collect::<Vec<_>>().join(",")));
    }
    // 按 submodule 聚合（rusty_h265 一仓两 patch 共用脏表）。
    let mut by_sub: BTreeMap<&str, Vec<&Job>> = BTreeMap::new();
    for j in jobs {
        by_sub.entry(j.sub).or_default().push(j);
    }
    let mut total = 0;
    for (sub, js) in &by_sub {
        process_sub(sub, js, check);
        total += js.len();
    }
    println!("patch-sync: {} ({total} job{})", if check { "check clean" } else { "regenerated" }, if total == 1 { "" } else { "s" });
}

/// 处理一个 submodule：scope 外脏文件报错；新增文件 `add -N`；逐 job 出 patch。
fn process_sub(sub: &str, jobs: &[&Job], check: bool) {
    // 全 union scope（同仓多 job 共享脏表，互不误伤）。
    let all_scope: Vec<&str> = jobs.iter().flat_map(|j| j.scope.iter().copied()).collect();
    let entries = git_status(sub);
    let mut in_scope: Vec<(char, String)> = vec![];
    let mut out_scope: Vec<String> = vec![];
    for (kind, path) in entries {
        if all_scope.iter().any(|s| path == *s || path.starts_with(*s)) {
            in_scope.push((kind, path));
        } else {
            out_scope.push(format!("{kind} {path}"));
        }
    }
    if !out_scope.is_empty() {
        fail(&format!(
            "{sub}: {} out-of-scope dirty file(s), refusing (extend scope in tools/patch-sync/src/main.rs if legit):\n  {}",
            out_scope.len(),
            out_scope.join("\n  ")
        ));
    }
    // 新增文件 intent-to-add，否则 `git diff` 静默吞掉（丢 patch 不丢树=断 fresh clone）。
    // 只在重打模式做：--check 是只读的，不许动 index。
    if !check {
        let untracked: Vec<String> = in_scope.iter().filter(|(k, _)| *k == '?').map(|(_, p)| p.clone()).collect();
        if !untracked.is_empty() {
            println!("{sub}: intent-to-add {} new file(s)", untracked.len());
            let mut args: Vec<&str> = vec!["add", "-N", "--"];
            args.extend(untracked.iter().map(String::as_str));
            git_run(sub, &args);
        }
    }
    for job in jobs {
        if check {
            check_job(sub, job);
            println!("{}: clean", job.name);
        } else {
            let mut args = vec!["diff".to_string(), "--".to_string()];
            args.extend(job.scope.iter().map(|s| s.trim_end_matches('/').to_string()));
            let text = git_out(sub, &args.iter().map(String::as_str).collect::<Vec<_>>());
            let patch_path = format!("patches/{}.patch", job.name);
            std::fs::write(&patch_path, text).unwrap_or_else(|e| fail(&format!("write {patch_path}: {e}")));
            println!("{}: wrote {}", job.name, patch_path);
        }
    }
}

/// `--check`：fresh-clone 模拟——`git archive HEAD` 取纯净 base，打上已提交 patch，
/// 与 submodule 工作树逐字节比对（只看本 job 作用域）。diff 算法无关，零误报。
fn check_job(sub: &str, job: &Job) {
    let base = std::env::temp_dir().join(format!("patch-sync-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap_or_else(|e| fail(&format!("tmpdir: {e}")));
    let root = std::env::current_dir().unwrap_or_else(|e| fail(&format!("cwd: {e}")));
    let git_dir = root.join(sub).join(".git");
    let mut scope: Vec<&str> = vec![];
    for s in job.scope {
        scope.push(s.trim_end_matches('/'));
    }
    // 1) 纯净 base 导出。
    let mut ac: Vec<&str> = vec!["--git-dir", git_dir.to_str().unwrap_or_else(|| fail("non-utf8 path")), "--work-tree", base.to_str().unwrap_or_else(|| fail("non-utf8 path")), "checkout", "HEAD", "--"];
    ac.extend(scope.iter().copied());
    let st = Command::new("git").args(&ac).status().unwrap_or_else(|e| fail(&format!("spawn git: {e}")));
    if !st.success() {
        fail(&format!("export base tree of {sub} failed"));
    }
    // 2) 已提交 patch 打上去（与 fresh-clone 流程同命令）。
    let patch_abs = root.join(format!("patches/{}.patch", job.name));
    let st = Command::new("git").arg("-C").arg(&base).arg("apply").arg(&patch_abs).status().unwrap_or_else(|e| fail(&format!("spawn git: {e}")));
    if !st.success() {
        fail(&format!("patches/{}.patch does not apply to pristine base (submodule moved?)", job.name));
    }
    // 3) 作用域内逐字节比对。
    for s in &scope {
        let a = base.join(s);
        let b = root.join(sub).join(s);
        let cmp = Command::new("diff").arg("-r").arg("-x").arg("target").arg("-x").arg("Cargo.lock").arg(&a).arg(&b).output().unwrap_or_else(|e| fail(&format!("spawn diff: {e}")));
        if !cmp.status.success() {
            fail(&format!(
                "patches/{}.patch: drift detected (submodule has unpatched changes under {s}); run without --check to regenerate:\n{}",
                job.name,
                String::from_utf8_lossy(&cmp.stdout).trim()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// `git status --porcelain=v1 -uall`：返回 (kind, submodule 内相对路径)。
/// kind: ' ' 已暂存改动 / 'M' 未暂存改动 / '?' 未跟踪（重命名/类型变更按改动计）。
fn git_status(sub: &str) -> Vec<(char, String)> {
    let out = git_out(sub, &["status", "--porcelain=v1", "-uall", "--", "."]);
    let mut v = vec![];
    for line in out.lines() {
        let (xy, path) = match (line.get(..2), line.get(3..)) {
            (Some(xy), Some(p)) => (xy, p),
            _ => continue,
        };
        // 处理 `old -> new` 重命名记法（取新路径）。
        let path = path.split(" -> ").last().unwrap_or(path);
        // 去掉 git 的引号转义（非 ASCII 路径）；本仓无此情况，保守处理。
        let path = path.trim_matches('"').to_string();
        let kind = if xy.starts_with('?') { '?' } else { 'M' };
        v.push((kind, path));
    }
    v
}

fn git_out(sub: &str, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(sub).args(args).output().unwrap_or_else(|e| fail(&format!("spawn git: {e}")));
    if !out.status.success() {
        fail(&format!("git -C {sub} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).unwrap_or_else(|e| fail(&format!("git output not utf-8: {e}")))
}

fn git_run(sub: &str, args: &[&str]) {
    let st = Command::new("git").arg("-C").arg(sub).args(args).status().unwrap_or_else(|e| fail(&format!("spawn git: {e}")));
    if !st.success() {
        fail(&format!("git -C {sub} {} failed", args.join(" ")));
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("patch-sync: error: {msg}");
    std::process::exit(1);
}
