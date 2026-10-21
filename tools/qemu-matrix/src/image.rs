//! `qemu-matrix::image`：FAT 镜像制作（`make_fat_image`/`populate`）与工件回读。
//! 系统工具只当子进程调（`run` 统一入口）。拆分仅为 ≤1000 行。

use super::args::glob_match;
use std::process::Command;

pub(crate) fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
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
pub(crate) fn make_fat_image(path: &str, size_mb: u32) -> Result<&'static str, String> {
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

pub(crate) fn populate(fstype: &str, image: &str, files: &[(&str, Vec<u8>)]) -> Result<(), String> {
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
pub(crate) fn read_artifact(fstype: &str, image: &str, glob: &str, contains: &[String]) -> (bool, String) {
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

