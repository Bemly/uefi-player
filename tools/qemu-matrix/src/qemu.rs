//! `qemu-matrix::qemu`：QEMU 进程管理（`run_case` 单 case 全流程）。
//! 组镜像 → 起 QEMU（pflash 挂 OVMF）→ 串口断言 + `sendkey` 按键 → 查工件。
//! 拆分仅为 ≤1000 行，不改行为。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::image::{make_fat_image, populate, read_artifact, run};
use super::model::{Case, Expect, Global};

use super::{ROOT, TOOL_DIR};

pub(crate) fn pick_accel() -> Vec<String> {
    if cfg!(target_os = "macos") {
        return vec!["-accel".into(), "hvf".into()];
    }
    if std::path::Path::new("/dev/kvm").exists() {
        return vec!["-accel".into(), "kvm".into()];
    }
    eprintln!("[warn] no hvf/kvm, falling back to tcg (slow)");
    vec!["-accel".into(), "tcg".into()]
}

pub(crate) fn qemu_bin() -> Result<String, String> {
    for c in ["qemu-system-x86_64", "/usr/local/bin/qemu-system-x86_64"] {
        if run("which", &[c]).is_ok() || std::path::Path::new(c).exists() {
            return Ok(c.into());
        }
    }
    Err("qemu-system-x86_64 not found".into())
}

/// 等全部字面模式在串口出现；qemu 提前退出即失败。
pub(crate) fn wait_patterns(
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
pub(crate) fn sendkey(port: u16, key: &str, timeout: Duration) -> bool {
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

pub(crate) fn sendkey_once(s: TcpStream, key: &str) -> std::io::Result<()> {
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

pub(crate) fn build_efi(force: bool) -> Result<String, String> {
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
pub(crate) fn resolve_ovmf(req: Option<&str>, debug_default: &str) -> String {
    match req {
        None | Some("debug") => debug_default.into(),
        Some("release") => format!("{TOOL_DIR}/assets/ovmf-release-x86_64-code.fd"),
        Some(p) => p.into(),
    }
}

pub(crate) fn run_case(
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
    if case.needs_raws > 0 {
        // 播片 raws 片源：`1080p/video.raws` 黑场 N 帧首尾相接（单文件路只走通路）。
        files.push((
            "1080p/video.raws",
            vec![0u8; case.needs_raws as usize * 1920 * 1080 * 4],
        ));
    }
    for i in 0..case.needs_rawvideo {
        // 播片 raw 片源黑场帧（`[play] format = "raw"` 用，同上只走通路）。
        files.push((
            Box::leak(format!("1080p/frame{i:04}.raw").into_boxed_str()),
            vec![0u8; 1920 * 1080 * 4],
        ));
    }
    for i in 0..case.needs_rawvideo720 {
        // RAW-GEOM：小分辨率 raw 片源黑场帧（`720p/` 独立目录，1280x720，
        // 配 `[decode.raw] width=1280 height=720` 用；几何不混）。
        files.push((
            Box::leak(format!("720p/frame{i:04}.raw").into_boxed_str()),
            vec![0u8; 1280 * 720 * 4],
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

