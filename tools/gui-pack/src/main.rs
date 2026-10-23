//! config-gui 打包工具（host，`std`）。
//!
//! 用法（与其他 tools 同式：显式 host target，根 `.cargo/config` 把默认钉成 UEFI）：
//! ```text
//! cargo run --manifest-path tools/gui-pack/Cargo.toml --target <host> -- make-icon <source.png> [assets_dir]
//! cargo run --manifest-path tools/gui-pack/Cargo.toml --target <host> -- bundle-macos [debug|release]
//! ```
//!
//! - `make-icon`：黑底像素风原图 → 按内容框裁留白 → 透明垫正方形，产
//!   `icon.png`（1024，NEAREST 保颗粒）/ `icon-256.rgba`（主程序 `include_bytes!`）/
//!   `icon.ico`（16-256 多尺寸）/ `icon.icns`（macOS 上经 `iconutil`，其他平台留 iconset）。
//! - `bundle-macos`：编 config-gui 后包成 `tools/target/bundle/UEFI Player.app`
//!   （Dock/访达名走 bundle：en `UEFI Player` / zh-Hans `UEFI播放器`，跟系统语言走；
//!   `cargo run` 是裸二进制，Dock 只能显示可执行文件名，看不到图标与包装名）。

use std::path::{Path, PathBuf};
use std::process::Command;

use image::imageops::FilterType;
use image::{ExtendedColorType, RgbaImage};

/// 本包目录（`tools/gui-pack`）；tools 根与 config-gui 都从它推。
const PKG_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("make-icon") => match args.get(1) {
            Some(src) => {
                let out = args
                    .get(2)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| tools_dir().join("config-gui/assets"));
                make_icon(Path::new(src), &out)
            }
            None => Err(String::from("make-icon 需要 <source.png>")),
        },
        Some("bundle-macos") => bundle_macos(args.get(1).map(String::as_str) == Some("release")),
        _ => Err(String::from(
            "用法：gui-pack make-icon <source.png> [assets_dir] | gui-pack bundle-macos [debug|release]",
        )),
    };
    if let Err(e) = r {
        eprintln!("gui-pack: {e}");
        std::process::exit(1);
    }
}

fn tools_dir() -> PathBuf {
    Path::new(PKG_DIR).join("..")
}

/// 内容框（非近黑像素，步长 4 采样，与原 PIL 脚本同口径）。
fn content_box(img: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = img.dimensions();
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let p = img.get_pixel(x, y).0;
            if p[0].max(p[1]).max(p[2]) > 12 {
                b = Some(match b {
                    None => (x, x, y, y),
                    Some((x0, x1, y0, y1)) => (x0.min(x), x1.max(x), y0.min(y), y1.max(y)),
                });
            }
        }
    }
    b
}

/// 裁内容框（均等边距 max(8, 4%)）后透明垫正方形。
fn squared(img: &RgbaImage) -> Result<RgbaImage, String> {
    let (w, h) = img.dimensions();
    let (x0, x1, y0, y1) = content_box(img).ok_or("原图全黑，找不到内容框")?;
    let m = 8.max(((x1 - x0).max(y1 - y0) as f32 * 0.04) as u32);
    let (cx0, cy0) = (x0.saturating_sub(m), y0.saturating_sub(m));
    let (cx1, cy1) = ((x1 + 1 + m).min(w), (y1 + 1 + m).min(h));
    let cropped = image::imageops::crop_imm(img, cx0, cy0, cx1 - cx0, cy1 - cy0).to_image();
    let side = cropped.width().max(cropped.height());
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut canvas,
        &cropped,
        ((side - cropped.width()) / 2) as i64,
        ((side - cropped.height()) / 2) as i64,
    );
    Ok(canvas)
}

fn nearest(img: &RgbaImage, s: u32) -> RgbaImage {
    image::imageops::resize(img, s, s, FilterType::Nearest)
}

fn make_icon(src: &Path, out: &Path) -> Result<(), String> {
    let img = image::open(src)
        .map_err(|e| format!("读 {}: {e}", src.display()))?
        .to_rgba8();
    let canvas = squared(&img)?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let save = |im: &RgbaImage, name: &str| {
        im.save(out.join(name))
            .map_err(|e| format!("写 {name}: {e}"))
    };
    save(&nearest(&canvas, 1024), "icon.png")?;
    let s256 = nearest(&canvas, 256);
    std::fs::write(out.join("icon-256.rgba"), s256.as_raw()).map_err(|e| e.to_string())?;
    // Windows 多尺寸合一（同 PIL `sizes=[16..256]`，由 256 图 NEAREST 降采）。
    let mut frames = Vec::new();
    for s in [16u32, 32, 48, 64, 128, 256] {
        let im = nearest(&s256, s);
        frames.push(
            image::codecs::ico::IcoFrame::as_png(im.as_raw(), s, s, ExtendedColorType::Rgba8)
                .map_err(|e| e.to_string())?,
        );
    }
    let ico = std::fs::File::create(out.join("icon.ico")).map_err(|e| e.to_string())?;
    image::codecs::ico::IcoEncoder::new(ico)
        .encode_images(&frames)
        .map_err(|e| format!("写 icon.ico: {e}"))?;
    let iconset = out.join("icon.iconset");
    std::fs::create_dir_all(&iconset).map_err(|e| e.to_string())?;
    for (s, name) in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ] {
        nearest(&canvas, s)
            .save(iconset.join(name))
            .map_err(|e| format!("写 {name}: {e}"))?;
    }
    if cfg!(target_os = "macos") {
        let st = Command::new("iconutil")
            .arg("-c")
            .arg("icns")
            .arg(&iconset)
            .arg("-o")
            .arg(out.join("icon.icns"))
            .status()
            .map_err(|e| format!("iconutil: {e}"))?;
        if !st.success() {
            return Err(String::from("iconutil 失败"));
        }
        std::fs::remove_dir_all(&iconset).map_err(|e| e.to_string())?;
        println!(
            "done: icon.png icon-256.rgba icon.ico icon.icns -> {}",
            out.display()
        );
    } else {
        println!(
            "done: icon.png icon-256.rgba icon.ico icon.iconset/ -> {}（非 macOS，icns 需在 mac 上 iconutil）",
            out.display()
        );
    }
    Ok(())
}

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>player-config-gui</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundleIdentifier</key>
    <string>dev.bemly.player-config-gui</string>
    <key>CFBundleName</key>
    <string>UEFI Player</string>
    <key>CFBundleDisplayName</key>
    <string>UEFI Player</string>
    <key>LSHasLocalizedDisplayName</key>
    <true/>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
"#;

fn host_triple() -> Result<String, String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("rustc -vV: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .ok_or_else(|| String::from("rustc -vV 无 host 行"))
}

fn bundle_macos(release: bool) -> Result<(), String> {
    let host = host_triple()?;
    let tools = tools_dir();
    let gui = tools.join("config-gui");
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(gui.join("Cargo.toml"))
        .arg("--target")
        .arg(&host);
    if release {
        cmd.arg("--release");
    }
    if !cmd.status().map_err(|e| format!("cargo: {e}"))?.success() {
        return Err(String::from("cargo build 失败"));
    }
    // tools workspace 共用 `tools/target`。
    let profile = if release { "release" } else { "debug" };
    let bin = tools
        .join("target")
        .join(&host)
        .join(profile)
        .join("player-config-gui");
    let app = tools.join("target/bundle/UEFI Player.app");
    let _ = std::fs::remove_dir_all(&app);
    let res = app.join("Contents/Resources");
    for d in [
        app.join("Contents/MacOS"),
        res.clone(),
        res.join("en.lproj"),
        res.join("zh-Hans.lproj"),
    ] {
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    }
    let copy = |from: &Path, to: &Path| {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| format!("拷 {}: {e}", from.display()))
    };
    copy(&bin, &app.join("Contents/MacOS/player-config-gui"))?;
    copy(&gui.join("assets/icon.icns"), &res.join("icon.icns"))?;
    let write = |p: PathBuf, s: &str| std::fs::write(&p, s).map_err(|e| e.to_string());
    write(app.join("Contents/Info.plist"), INFO_PLIST)?;
    // 本地化 Dock/访达名（跟系统语言走，不跟应用内中英切换联动；窗口标题才跟，见 app.rs）。
    write(
        res.join("en.lproj/InfoPlist.strings"),
        "\"CFBundleName\" = \"UEFI Player\";\n\"CFBundleDisplayName\" = \"UEFI Player\";\n",
    )?;
    write(
        res.join("zh-Hans.lproj/InfoPlist.strings"),
        "\"CFBundleName\" = \"UEFI播放器\";\n\"CFBundleDisplayName\" = \"UEFI播放器\";\n",
    )?;
    println!("bundled: {}", app.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squared_crops_black_border_and_pads() {
        // 100×60 黑底，中间 20×10 白块：裁框 + 边距 8 后垫成正方形。
        let mut img = RgbaImage::from_pixel(100, 60, image::Rgba([0, 0, 0, 255]));
        for y in 24..34 {
            for x in 40..60 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sq = squared(&img).unwrap();
        assert_eq!(sq.width(), sq.height());
        assert!(sq.width() < 100, "黑边应被裁掉：{}", sq.width());
    }

    #[test]
    fn squared_rejects_all_black() {
        let img = RgbaImage::from_pixel(16, 16, image::Rgba([0, 0, 0, 255]));
        assert!(squared(&img).is_err());
    }
}
