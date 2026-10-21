//! QEMU 黑盒矩阵 runner（测试代码 100% Rust，见 AGENTS）。
//!
//! 每 case 独立镜像 + 独立 QEMU 进程：塞 `player.toml` → 抓串口 → 发按键 →
//! 断言。串口匹配是字面包含（非正则），通配只支持 `*`（够 `HWBENCH*.TXT` 用）。
//! 系统工具（qemu/OVMF/hdiutil/mtools）只当子进程调，不写测试逻辑。

use serde::Deserialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 编译期钉住仓库根：runner 从哪跑都找到 matrix.toml。
const TOOL_DIR: &str = env!("CARGO_MANIFEST_DIR");
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn default_wait() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct Matrix {
    #[serde(default)]
    global: Global,
    #[serde(default)]
    case: Vec<Case>,
}

#[derive(Debug, Deserialize, Default)]
struct Global {
    smp: Option<u32>,
    mem_mb: Option<u32>,
    ovmf: Option<String>,
    /// 默认 CPU 模型：缺省不用 `-cpu`（QEMU 默认 qemu64，SSE2 基线，门禁口径）；
    /// "host" = 宿主透传（本机 i5-10400 即 AVX2），其它按 QEMU 模型名原样传。
    cpu: Option<String>,
    timeout_sec: Option<u64>,
    settle_sec: Option<u64>,
    #[serde(default)]
    forbid: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    config: String,
    #[serde(default)]
    boot_expect: Vec<Expect>,
    #[serde(default)]
    keys: Vec<KeyStep>,
    #[serde(default)]
    expect: Vec<Expect>,
    forbid: Option<Vec<String>>,
    timeout_sec: Option<u64>,
    settle_sec: Option<u64>,
    #[serde(default)]
    needs_video: bool,
    #[serde(default)]
    needs_wallpaper: u32,
    /// 播片 raw 片源：`1080p/frameNNNN.raw` 黑场 N 帧（`[play] format = "raw"` 用）。
    #[serde(default)]
    needs_rawvideo: u32,
    /// 壁纸 qois 片源：把测试视频拷成 `wallpaper/wall.qois`
    ///（`[wallpaper] format = "qois"` 用，验开机预解码 + 循环）。
    #[serde(default)]
    needs_wallpaper_qois: bool,
    /// 播片 hevc 片源：探针样片拷成 `1080p/video.hevc`
    ///（`[play] format = "hevc"` 用，Annex-B 软解进 Clip）。
    #[serde(default)]
    needs_hevcvideo: bool,
    /// 播片 vp9 片源：探针样片拷成 `1080p/video.ivf`
    ///（`[play] format = "vp9"` 用，IVF 软解进 Clip）。
    #[serde(default)]
    needs_vp9video: bool,
    /// 播片 avc 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "avc"` 用，石雨堂拆 avc 轨进 Clip）。
    #[serde(default)]
    needs_avcvideo: bool,
    /// 播片 hevc-MP4 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "hevc"` 的容器路，用石雨堂拆 hev 轨进 Clip）。
    #[serde(default)]
    needs_hevcmp4video: bool,
    /// 播片 vp9-MP4 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "vp9"` 的容器路，用石雨堂拆 vp09 轨进 Clip）。
    #[serde(default)]
    needs_vp9mp4video: bool,
    /// 播片 h263 片源：裸流样片拷成 `1080p/video.h263`
    ///（`[play] format = "h263"` 用，直解进 Clip）。
    #[serde(default)]
    needs_h263video: bool,
    /// 播片 mpeg2v 片源：裸基本流样片拷成 `1080p/video.m2v`
    ///（`[play] format = "mpeg2v"` 用，接入单元切分进 Clip）。
    #[serde(default)]
    needs_mpeg2vvideo: bool,
    /// 播片 theora 片源：Ogg 样片拷成 `1080p/video.ogv`
    ///（`[play] format = "theora"` 用，手写拆包进 Clip）。
    #[serde(default)]
    needs_theoravideo: bool,
    /// 壁纸 avc 片源：1080p MP4 样片拷成 `wallpaper/wall.mp4`
    ///（`[wallpaper] format = "avc"` 用，验壁纸侧容器拆轨 + 预载）。
    #[serde(default)]
    needs_avcwallpaper: bool,
    /// chainload 自举：镜像里再放一份 efi + 一条 BLS entry（单盘场景）。
    #[serde(default)]
    needs_chainload: bool,
    #[serde(default)]
    slow: bool,
    soak_sec: Option<u64>,
    artifact: Option<Artifact>,
    /// 本 case 的固件：缺省跟全局/命令行；"debug" = brew 瓶里的 DEBUG OVMF
    ///（带 ASSERT，抓固件 bug），"release" = 自带 nightly RELEASE
    ///（`assets/ovmf-release-x86_64-code.fd`，无 ASSERT、更快，适合比性能），
    /// 其它值按路径原样用。命令行 `--ovmf/--ovmf-set` 优先于此。
    ovmf: Option<String>,
    /// 本 case 的 CPU 模型：缺省跟全局/命令行；"host" 透传宿主特性，其它按名原样传。
    /// 命令行 `--cpu` 优先于此。
    cpu: Option<String>,
    /// `--changed` 覆盖声明：本 case 锻炼到的仓库根相对路径前缀（文件或目录），
    /// 改动落到其中即选中本 case；`"*"` = 任何改动都跑（只给 menu-smoke 锚点用）。
    /// 缺省空 = 只靠隐式核心（见 CORE）选中，即“纯标定类”case。
    /// 维护：改某子系统代码时顺手核对对应 case 的 covers（和 AGENTS 黑盒规则配套）。
    #[serde(default)]
    covers: Vec<String>,
    /// 附加卷（第二盘及以后）：多卷自动发现的测试夹具，按顺序挂 hd2、hd3…。
    #[serde(default)]
    extra_volumes: Vec<ExtraVolume>,
}

/// 断言项：纯字串 = 出现一次即满足；`{pat,count}` = 出现次数达标
///（chainload 这类"同一镜像起两次"的场景，靠计数区分第几轮）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Expect {
    Once(String),
    Count { pat: String, count: usize },
}

impl Expect {
    fn satisfied(&self, data: &str) -> bool {
        match self {
            Expect::Once(p) => data.contains(p),
            Expect::Count { pat, count } => data.matches(pat).count() >= *count,
        }
    }
    fn describe(&self) -> String {
        match self {
            Expect::Once(p) => format!("{p:?}"),
            Expect::Count { pat, count } => format!("{pat:?} x{count}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct KeyStep {
    send: String,
    #[serde(default = "default_wait")]
    wait_after: u64,
    /// 发键前先等这些串口断言（事件驱动，不靠裸睡；超时即 FAIL）。
    /// 屏显-only 且无标记的界面填空 + `wait_after` 估，case 里注明。
    #[serde(default)]
    wait_for: Vec<Expect>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    glob: String,
    #[serde(default)]
    contains: Vec<String>,
    /// 查哪个盘的落盘工件：缺省 1 = 主盘；2 = 第一附加卷，依此类推
    ///（boot counting 夹具在第二盘，`boot-counting` case 用）。
    #[serde(default = "default_artifact_volume")]
    volume: usize,
}

fn default_artifact_volume() -> usize {
    1
}

/// 附加卷（第二盘及以后）：多卷自动发现的测试夹具。
/// 文件来源三选一：`text` 内联文本 / `zeros` 全零占位 / `efi = true` 拷被测 efi。
#[derive(Debug, Deserialize)]
struct ExtraFile {
    path: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    zeros: Option<usize>,
    #[serde(default)]
    efi: bool,
    #[serde(default)]
    asset: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExtraVolume {
    #[serde(default)]
    files: Vec<ExtraFile>,
}

#[derive(Debug, Default)]
struct Args {
    list: bool,
    case: Option<String>,
    include_slow: bool,
    keep: bool,
    no_build: bool,
    /// 只跑改动波及的 case（迭代加速；提交门禁仍是全矩阵）。
    /// 改动 = 工作区未提交（含 untracked）∪ 分支相对 origin/master 的已提交。
    changed: bool,
    ovmf: Option<String>,
    /// 强制本轮全用指定 CPU（模型名或 "host"）；单 case `cpu` 与全局 `cpu` 照认，
    /// 优先级：命令行 > 单 case > 全局 > 缺省（不用 -cpu，即 qemu64 基线）。
    cpu: Option<String>,
    /// 全局固件档：debug（默认，沿用全局/单 case 设置）、release（全用 RELEASE 跑一遍）、
    /// both（每个 case 跑两遍，第二遍挂 `-release` 后缀；与 `--ovmf` 同给时以它为准）。
    ovmf_set: Option<String>,
}

fn parse_args() -> Args {
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
const CORE: &[&str] = &[
    "src/main.rs",
    "src/menu.rs",
    "src/idle.rs",
    "src/graphics.rs",
    "src/diag/common.rs",
    "src/config/",
    "src/fs.rs",
    "src/error.rs",
];

/// 前缀命中：改动文件等于该前缀，或落在其目录下（`"src/boot/"` 盖住
/// `src/boot/chainload.rs`；文件前缀如 `"src/video.rs"` 走等于分支）。
fn prefixes_hit<'a>(covers: impl IntoIterator<Item = &'a str>, changed: &[String]) -> bool {
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

fn case_hit(covers: &[String], changed: &[String]) -> bool {
    prefixes_hit(covers.iter().map(String::as_str), changed)
        || prefixes_hit(CORE.iter().copied(), changed)
}

/// 收集改动文件（仓库根相对路径）：工作区未提交（含 untracked `??` 与改名后者）
/// ∪ 分支相对 origin/master 的已提交。git 不可用返回 `None`（调用方全跑）。
/// `third_party/` 工作区恒脏（patches 叠加态）只在 status 侧忽略；
/// 分支 diff 侧保留（pin 变更能被看见，见下 force_full_reason）。
fn changed_files() -> Option<Vec<String>> {
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
fn force_full_reason(changed: &[String]) -> Option<&'static str> {
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
fn glob_match(pat: &str, name: &str) -> bool {
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

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 建空 FAT 镜像。macOS 走 hdiutil+newfs_msdos（已验证），Linux 走 mtools。
fn make_fat_image(path: &str, size_mb: u32) -> Result<&'static str, String> {
    if cfg!(target_os = "macos") {
        run("mkfile", &["-n", &format!("{size_mb}m"), path])?;
        let out = run("hdiutil", &["attach", "-nomount", path])?;
        let dev = out
            .split_whitespace()
            .find(|t| t.starts_with("/dev/"))
            .ok_or_else(|| format!("hdiutil attach gave no device: {out}"))?
            .to_string();
        let r = run("newfs_msdos", &["-F", "32", "-v", "UEFIPLAYER", &dev]);
        let _ = run("hdiutil", &["detach", &dev]);
        r?;
        return Ok("hdiutil");
    }
    if run("which", &["mformat"]).is_ok() {
        run(
            "mformat",
            &["-i", path, "-C", "-F", "-v", "UEFIPLAYER", "::"],
        )?;
        return Ok("mtools");
    }
    Err("no FAT tool: macOS needs hdiutil/newfs_msdos, linux needs mtools".into())
}

fn populate(fstype: &str, image: &str, files: &[(&str, Vec<u8>)]) -> Result<(), String> {
    if fstype == "hdiutil" {
        let mnt = format!("{image}.mnt");
        let _ = std::fs::create_dir_all(&mnt);
        run("hdiutil", &["attach", "-mountpoint", &mnt, image])?;
        let mut ok: Result<(), String> = Ok(());
        for (arc, data) in files {
            let dst = format!("{mnt}/{arc}");
            if let Some(p) = std::path::Path::new(&dst).parent() {
                if std::fs::create_dir_all(p).is_err() {
                    ok = Err(format!("mkdir {p:?}"));
                    break;
                }
            }
            if std::fs::write(&dst, data).is_err() {
                ok = Err(format!("write {dst}"));
                break;
            }
        }
        let _ = run("hdiutil", &["detach", &mnt]);
        return ok;
    }
    for (arc, data) in files {
        let stage = format!("{image}.stage");
        std::fs::write(&stage, data).map_err(|e| format!("stage: {e}"))?;
        let parts: Vec<&str> = arc.split('/').collect();
        for i in 1..parts.len() {
            let dir = format!("::/{}", parts[..i].join("/"));
            let _ = run("mmd", &["-i", image, &dir]);
        }
        run("mcopy", &["-i", image, "-o", &stage, &format!("::/{arc}")])?;
        let _ = std::fs::remove_file(&stage);
    }
    Ok(())
}

/// 重挂镜像找工件（只读）。返回 (ok, detail)。
fn read_artifact(fstype: &str, image: &str, glob: &str, contains: &[String]) -> (bool, String) {
    let check = |name: &str, data: &[u8]| -> Option<String> {
        let text = String::from_utf8_lossy(data);
        for c in contains {
            if !text.contains(c) {
                return Some(format!("{name} missing {c:?}"));
            }
        }
        None
    };
    if fstype == "hdiutil" {
        let mnt = format!("{image}.ro");
        let _ = std::fs::create_dir_all(&mnt);
        if run(
            "hdiutil",
            &["attach", "-readonly", "-mountpoint", &mnt, image],
        )
        .is_err()
        {
            return (false, "re-mount failed".into());
        }
        let mut hits = Vec::new();
        let mut stack = vec![mnt.clone()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p.to_string_lossy().into_owned());
                    } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                        if glob_match(glob, n) {
                            hits.push(p);
                        }
                    }
                }
            }
        }
        let mut detail = format!("no file matching {glob}");
        let mut ok = false;
        for h in &hits {
            match std::fs::read(h) {
                Ok(data) => match check(&h.to_string_lossy(), &data) {
                    None => {
                        ok = true;
                        detail = format!("{} file(s) ok", hits.len());
                        break;
                    }
                    Some(d) => detail = d,
                },
                Err(e) => detail = format!("read: {e}"),
            }
        }
        let _ = run("hdiutil", &["detach", &mnt]);
        return (ok, detail);
    }
    let list = match run("mdir", &["-i", image, "-b", "::/"]) {
        Ok(o) => o,
        Err(e) => return (false, e),
    };
    let names: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|n| glob_match(glob, n))
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return (false, format!("no file matching {glob}"));
    }
    for n in &names {
        let tmp = format!("{image}.ro");
        if run("mcopy", &["-i", image, &format!("::/{n}"), &tmp]).is_err() {
            return (false, format!("read {n}"));
        }
        let data = std::fs::read(&tmp).unwrap_or_default();
        let _ = std::fs::remove_file(&tmp);
        if let Some(d) = check(n, &data) {
            return (false, d);
        }
    }
    (true, format!("{} file(s) ok", names.len()))
}

fn pick_accel() -> Vec<String> {
    if cfg!(target_os = "macos") {
        return vec!["-accel".into(), "hvf".into()];
    }
    if std::path::Path::new("/dev/kvm").exists() {
        return vec!["-accel".into(), "kvm".into()];
    }
    eprintln!("[warn] no hvf/kvm, falling back to tcg (slow)");
    vec!["-accel".into(), "tcg".into()]
}

fn qemu_bin() -> Result<String, String> {
    for c in ["qemu-system-x86_64", "/usr/local/bin/qemu-system-x86_64"] {
        if run("which", &[c]).is_ok() || std::path::Path::new(c).exists() {
            return Ok(c.into());
        }
    }
    Err("qemu-system-x86_64 not found".into())
}

/// 等全部字面模式在串口出现；qemu 提前退出即失败。
fn wait_patterns(
    path: &str,
    patterns: &[Expect],
    timeout: Duration,
    proc: &mut Child,
) -> (bool, Vec<String>, String) {
    let start = Instant::now();
    let mut data: String;
    loop {
        if let Ok(Some(status)) = proc.try_wait() {
            data = std::fs::read_to_string(path).unwrap_or_default();
            return (
                false,
                vec![format!("<qemu exited rc={}>", status.code().unwrap_or(-1))],
                data,
            );
        }
        data = std::fs::read_to_string(path).unwrap_or_default();
        if patterns.iter().all(|p| p.satisfied(&data)) {
            return (true, vec![], data);
        }
        if start.elapsed() >= timeout {
            let missing: Vec<String> = patterns
                .iter()
                .filter(|p| !p.satisfied(&data))
                .map(|p| p.describe())
                .collect();
            return (false, missing, data);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// human monitor 发键（telnet + `sendkey`）。
fn sendkey(port: u16, key: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => {
                if let Err(e) = sendkey_once(s, key) {
                    eprintln!("  [monitor] send failed: {e}");
                    return false;
                }
                return true;
            }
            Err(_) => {
                if start.elapsed() >= timeout {
                    eprintln!("  [monitor] connect failed");
                    return false;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

fn sendkey_once(s: TcpStream, key: &str) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut s = s;
    let mut buf = vec![0u8; 4096];
    let mut banner = Vec::new();
    loop {
        // 等 banner 里的 (qemu) 提示符。
        let n = s.read(&mut buf)?;
        if n == 0 {
            break;
        }
        banner.extend_from_slice(&buf[..n]);
        if banner.windows(6).any(|w| w == b"(qemu)") {
            break;
        }
        if banner.len() > 65536 {
            break;
        }
    }
    s.write_all(format!("sendkey {key}\n").as_bytes())?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

fn build_efi(force: bool) -> Result<String, String> {
    // 默认必编（黑盒测的是当前代码，陈旧 EFI 会导致全员回退默认还很难查）；
    // 只有 --no-build 才复用盘上现有文件（要求它已存在）。
    if !force {
        for p in [
            format!("{ROOT}/target/x86_64-unknown-uefi/debug/uefi-player.efi"),
            format!("{ROOT}/target/x86_64-unknown-uefi/release/uefi-player.efi"),
        ] {
            if std::path::Path::new(&p).exists() {
                return Ok(p);
            }
        }
    }
    println!("[build] cargo build --target x86_64-unknown-uefi (debug)");
    let out = Command::new("cargo")
        .args(["build", "--target", "x86_64-unknown-uefi"])
        .current_dir(ROOT)
        .output()
        .map_err(|e| format!("spawn cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(format!(
        "{ROOT}/target/x86_64-unknown-uefi/debug/uefi-player.efi"
    ))
}

/// 固件路径解析："debug" = 全局默认（brew 瓶 DEBUG OVMF，`matrix.toml [global] ovmf`
/// 可改），"release" = 自带 nightly RELEASE（`assets/` 下，无 ASSERT），其它按路径原样用。
fn resolve_ovmf(req: Option<&str>, debug_default: &str) -> String {
    match req {
        None | Some("debug") => debug_default.into(),
        Some("release") => format!("{TOOL_DIR}/assets/ovmf-release-x86_64-code.fd"),
        Some(p) => p.into(),
    }
}

fn run_case(
    case: &Case,
    glob: &Global,
    efi: &str,
    keep: bool,
    ovmf: &str,
    cpu: &str,
    suffix: &str,
) -> (bool, String) {
    let tag = format!("{}{}", case.name, suffix);
    println!("=== case {tag} ===");
    let rundir = format!("{TOOL_DIR}/runs/{tag}");
    if std::path::Path::new(&rundir).exists() && !keep {
        let _ = std::fs::remove_dir_all(&rundir);
    }
    let _ = std::fs::create_dir_all(&rundir);
    let serial = format!("{rundir}/serial.log");
    let image = format!("{rundir}/usb.img");
    let _ = std::fs::remove_file(&image);

    // 1. 组镜像文件。
    let efi_data = std::fs::read(efi)
        .map_err(|e| format!("read efi: {e}"))
        .unwrap_or_default();
    if efi_data.is_empty() {
        return (false, "efi unreadable".into());
    }
    let mut files: Vec<(&str, Vec<u8>)> = vec![
        ("EFI/BOOT/BOOTX64.EFI", efi_data.clone()),
        (
            "player.toml",
            case.config.trim_start_matches('\n').as_bytes().to_vec(),
        ),
    ];
    if case.needs_video {
        let src = format!("{ROOT}/usb_reports/1080p/video.qois");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.qois", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    for i in 0..case.needs_wallpaper {
        // 黑场 BGRA 帧（1920x1080x4），只走通路不断言画质。
        files.push((
            Box::leak(format!("wallpaper/frame{i:04}.raw").into_boxed_str()),
            vec![0u8; 1920 * 1080 * 4],
        ));
    }
    for i in 0..case.needs_rawvideo {
        // 播片 raw 片源黑场帧（`[play] format = "raw"` 用，同上只走通路）。
        files.push((
            Box::leak(format!("1080p/frame{i:04}.raw").into_boxed_str()),
            vec![0u8; 1920 * 1080 * 4],
        ));
    }
    if case.needs_wallpaper_qois {
        // 壁纸 qois 片源：复用测试视频（真 QOI 帧，验预解码 + 循环）。
        let src = format!("{ROOT}/usb_reports/1080p/video.qois");
        match std::fs::read(&src) {
            Ok(d) => files.push(("wallpaper/wall.qois", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_hevcvideo {
        // 播片 hevc 片源：探针样片（Annex-B，320x240×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/hevc_probe_main.265");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.hevc", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_vp9video {
        // 播片 vp9 片源：探针样片（IVF，160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/vp9_probe.ivf");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.ivf", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_avcvideo {
        // 播片 avc 片源：MP4 样片（avc 轨，160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video-avc.mp4");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.mp4", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_hevcmp4video {
        // 播片 hevc-MP4 片源：MP4 样片（hev 轨，160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video-hevc.mp4");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.mp4", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_vp9mp4video {
        // 播片 vp9-MP4 片源：MP4 样片（vp09 轨，160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video-vp9.mp4");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.mp4", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_h263video {
        // 播片 h263 片源：裸流样片（QCIF 128x96×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video.h263");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.h263", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_mpeg2vvideo {
        // 播片 mpeg2v 片源：裸基本流样片（160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video.m2v");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.m2v", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_theoravideo {
        // 播片 theora 片源：Ogg 样片（160x120×10 帧，只走通路不断言画质）。
        let src = format!("{ROOT}/src/diag/data/video.ogv");
        match std::fs::read(&src) {
            Ok(d) => files.push(("1080p/video.ogv", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_avcwallpaper {
        // 壁纸 avc 片源：1080p MP4 样片拷成 `wallpaper/wall.mp4`
        //（验壁纸侧容器拆轨 + 预载截断，不断言画质）。
        let src = format!("{ROOT}/src/diag/data/wall-avc.mp4");
        match std::fs::read(&src) {
            Ok(d) => files.push(("wallpaper/wall.mp4", d)),
            Err(_) => return (false, format!("missing {src}")),
        }
    }
    if case.needs_chainload {
        // 被起镜像：同一份 efi 换个路径（单盘场景；跨盘见 extra_volumes 的 case）。
        let app2 = efi_data.clone();
        files.push(("EFI/other/app.efi", app2));
        files.push((
            "loader/entries/test.conf",
            b"title Self Chainload Test\nefi /EFI/other/app.efi\n".to_vec(),
        ));
    }
    let fstype = match make_fat_image(&image, 128) {
        Ok(t) => t,
        Err(e) => return (false, e),
    };
    if let Err(e) = populate(fstype, &image, &files) {
        return (false, e);
    }
    // 附加卷（第二盘起）：多卷自动发现的测试夹具。
    let mut extra_images = Vec::new();
    for (vi, vol) in case.extra_volumes.iter().enumerate() {
        let vimg = format!("{rundir}/usb{}.img", vi + 2);
        let _ = std::fs::remove_file(&vimg);
        let mut vfiles: Vec<(&str, Vec<u8>)> = Vec::new();
        for f in &vol.files {
            let leak: &str = Box::leak(f.path.clone().into_boxed_str());
            if f.efi {
                vfiles.push((leak, efi_data.clone()));
            } else if let Some(t) = &f.text {
                vfiles.push((leak, t.as_bytes().to_vec()));
            } else if let Some(n) = f.zeros {
                vfiles.push((leak, vec![0u8; n]));
            } else if let Some(a) = &f.asset {
                let src = format!("{ROOT}/{a}");
                match std::fs::read(&src) {
                    Ok(d) => vfiles.push((leak, d)),
                    Err(_) => return (false, format!("missing asset {src}")),
                }
            } else {
                return (false, format!("extra file {} has no source", f.path));
            }
        }
        if let Err(e) = make_fat_image(&vimg, 128) {
            return (false, e);
        }
        if let Err(e) = populate(fstype, &vimg, &vfiles) {
            return (false, e);
        }
        extra_images.push(vimg);
    }

    // 2. 起 QEMU。
    let port: u16 = 44000 + (tag.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32)) % 1000) as u16;
    let mut cmd = vec![
        qemu_bin().unwrap_or_else(|e| {
            println!("{e}");
            std::process::exit(2);
        }),
        "-M".into(),
        "q35".into(),
    ];
    // 缺省不用 -cpu（qemu64/SSE2 基线，门禁口径）；"host" 透传宿主特性测 AVX2。
    if !cpu.is_empty() {
        cmd.push("-cpu".into());
        cmd.push(cpu.into());
    }
    cmd.extend([
        "-device".into(),
        "virtio-vga,xres=1920,yres=1080".into(),
        "-drive".into(),
        format!("if=pflash,format=raw,unit=0,file={ovmf},readonly=on"),
        "-drive".into(),
        format!("file={image},format=raw,if=none,id=hd"),
        "-device".into(),
        "virtio-blk-pci,drive=hd".into(),
    ]);
    // 附加卷挂 hd2、hd3…（多卷发现的测试夹具）。
    for (i, vimg) in extra_images.iter().enumerate() {
        let id = format!("hd{}", i + 2);
        cmd.push("-drive".into());
        cmd.push(format!("file={vimg},format=raw,if=none,id={id}"));
        cmd.push("-device".into());
        cmd.push(format!("virtio-blk-pci,drive={id}"));
    }
    cmd.extend([
        "-smp".to_string(),
        glob.smp.unwrap_or(4).to_string(),
        "-m".into(),
        glob.mem_mb.unwrap_or(1024).to_string(),
        "-display".into(),
        "none".into(),
        "-no-reboot".into(),
        "-serial".into(),
        format!("file:{serial}"),
        "-monitor".into(),
        format!("telnet:127.0.0.1:{port},server,nowait"),
    ]);
    cmd.extend(pick_accel());
    println!("  {}", cmd.join(" "));
    let mut proc = match Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(p) => p,
        Err(e) => return (false, format!("spawn qemu: {e}")),
    };
    let timeout = Duration::from_secs(case.timeout_sec.or(glob.timeout_sec).unwrap_or(240));
    let forbid: &[String] = case.forbid.as_deref().unwrap_or(&glob.forbid);
    let t0 = Instant::now();
    let mut fail: Option<String> = None;

    // 3. 等 boot 断言 -> 落定（菜单重画+排空陈旧键约 1s 量级，首键不等会被 flush 吃掉）
    //    -> 发键 -> 等最终断言（可 soak）。
    let (ok, missing, _) = wait_patterns(&serial, &case.boot_expect, timeout, &mut proc);
    if !ok {
        fail = Some(format!("boot patterns missing: {missing:?}"));
    } else {
        let settle = Duration::from_secs(case.settle_sec.or(glob.settle_sec).unwrap_or(8));
        std::thread::sleep(settle);
        for k in &case.keys {
            if fail.is_some() {
                break;
            }
            if !k.wait_for.is_empty() {
                let remain_k = timeout
                    .saturating_sub(t0.elapsed())
                    .max(Duration::from_secs(10));
                let (ok, missing, _) = wait_patterns(&serial, &k.wait_for, remain_k, &mut proc);
                if !ok {
                    fail = Some(format!(
                        "key '{}' precondition missing: {missing:?}",
                        k.send
                    ));
                    break;
                }
            }
            if !sendkey(port, &k.send, Duration::from_secs(15)) {
                fail = Some(format!("sendkey {} failed", k.send));
                break;
            }
            std::thread::sleep(Duration::from_secs(k.wait_after));
        }
        if fail.is_none() {
            let elapsed = t0.elapsed();
            let remain = timeout.saturating_sub(elapsed).max(Duration::from_secs(10));
            let (ok, missing, _) = wait_patterns(&serial, &case.expect, remain, &mut proc);
            if !ok {
                fail = Some(format!("expect patterns missing: {missing:?}"));
            } else if let Some(soak) = case.soak_sec {
                println!("  soaking {soak}s...");
                std::thread::sleep(Duration::from_secs(soak));
            }
        }
    }
    let data = std::fs::read_to_string(&serial).unwrap_or_default();
    if fail.is_none() {
        for pat in forbid {
            if data.contains(pat) {
                fail = Some(format!("forbidden pattern: {pat:?}"));
                break;
            }
        }
    }
    if fail.is_none() {
        if let Some(art) = &case.artifact {
            // 1 = 主盘，2 起 = 附加卷（boot counting 夹具在第二盘）。
            let apath: Option<&str> = if art.volume.max(1) == 1 {
                Some(&image)
            } else {
                extra_images.get(art.volume - 2).map(String::as_str)
            };
            match apath {
                None => fail = Some(format!("artifact: volume {} has no image", art.volume)),
                Some(p) => {
                    let (ok, detail) = read_artifact(fstype, p, &art.glob, &art.contains);
                    println!("  artifact: {detail}");
                    if !ok {
                        fail = Some(format!("artifact: {detail}"));
                    }
                }
            }
        }
    }
    let _ = proc.kill();
    let _ = proc.wait();
    match fail {
        Some(f) => {
            println!("  FAIL: {f}");
            (false, f)
        }
        None => {
            println!("  PASS");
            (true, String::new())
        }
    }
}

fn main() {
    let args = parse_args();
    let text = std::fs::read_to_string(format!("{TOOL_DIR}/matrix.toml")).unwrap_or_else(|e| {
        eprintln!("read matrix.toml: {e}");
        std::process::exit(2);
    });
    let matrix: Matrix = toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("parse matrix.toml: {e}");
        std::process::exit(2);
    });
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
    use super::{case_hit, force_full_reason, prefixes_hit};

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
