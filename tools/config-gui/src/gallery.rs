//! 图库页：与 `player.toml` 同目录的媒体浏览（只读展示 + 选转码源）。
//!
//! 图库目录 = 已载入 `player.toml` 的父目录（默认 `"player.toml"` → `"."`），
//!   与配置同盘同目录，U盘上即插即看。固件认的布局是 `1080p/video.*`（播片）
//!   + `wallpaper/*`（壁纸），页内只列这两处的文件，不递归整盘。
//!
//!   控件只用玻璃件（`GlassButton` 选行/刷新跳转），纯文本走 label/monospace。

use crate::app::App;

/// 图库一行（文件名 + 字节数 + 固件侧格式标签）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GalleryEntry {
    pub(crate) name: String,
    /// 相对图库根的显示路径（如 `1080p/video.mp4`）。
    pub(crate) size: u64,
    pub(crate) kind: &'static str,
}

/// 图库根目录：`player.toml` 所在目录（`rfd` 存盘/读盘都落这里，天然同目录）。
/// 裸文件名（无父目录）按当前目录处理。
pub(crate) fn gallery_dir(player_path: &str) -> String {
    match std::path::Path::new(player_path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.display().to_string(),
        _ => String::from("."),
    }
}

/// 文件名 → 固件格式标签（纯规则，无 IO，可进黑盒矩阵）。
/// `video.mp4` 这类容器不落单格式，标 `mp4` 由转码页按轨识别。
pub(crate) fn media_kind(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    if base == "video.qois" || base == "wall.qois" {
        return "qois";
    }
    if base.starts_with("frame") && base.ends_with(".raw") {
        return "raw";
    }
    match base.rsplit('.').next().unwrap_or("") {
        "mp4" => "mp4",
        "hevc" | "265" => "hevc",
        "ivf" => "vp9",
        "h263" => "h263",
        "m2v" => "mpeg2v",
        "ogv" => "theora",
        "qois" => "qois",
        "raw" => "raw",
        _ => "other",
    }
}

/// 列图库：`1080p/` + `wallpaper/` 下的 regular 文件（大小 + 标签）。
/// 缺目录/读错静默空集（图库是装饰，不断任何流程）。
pub(crate) fn list_gallery(dir: &str) -> Vec<GalleryEntry> {
    let mut out = Vec::new();
    for sub in ["1080p", "wallpaper"] {
        let root = std::path::Path::new(dir).join(sub);
        let rd = match std::fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let meta = match ent.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let name = ent.file_name().to_string_lossy().into_owned();
            out.push(GalleryEntry {
                name: format!("{sub}/{name}"),
                size: meta.len(),
                kind: media_kind(&name),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 图库状态（`App` 持有；`dir` 随 `player.toml` 路径走，换文件即换库）。
pub(crate) struct GalleryState {
    pub(crate) entries: Vec<GalleryEntry>,
    pub(crate) selected: Option<usize>,
}

impl GalleryState {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: None,
        }
    }

    pub(crate) fn refresh(&mut self, player_path: &str) {
        let dir = gallery_dir(player_path);
        self.entries = list_gallery(&dir);
        self.selected = None;
    }
}

impl App {
    /// 图库页：目录行 + 刷新 + 文件行（点行即选中，转码按钮带源跳转）。
    pub(crate) fn gallery_page(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        use rust_i18n::t;
        self.content_heading(ui, t!("gallery.heading"));
        self.content_small(ui, t!("gallery.hint"));
        ui.add_space(8.0);
        let dir = gallery_dir(&self.path);
        ui.horizontal(|ui| {
            self.content_label(ui, t!("gallery.dir"));
            self.content_monospace(ui, dir.as_str());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if egui_glass::GlassButton::new(t!("gallery.refresh"))
                .show(ui)
                .clicked()
            {
                let p = self.path.clone();
                self.gallery.refresh(&p);
                self.status = t!("gallery.refreshed", n = self.gallery.entries.len()).into_owned();
            }
            // 选中行一键带源跳转转码页（源路径 = 图库根 + 相对名）。
            let can_go = self.gallery.selected.is_some();
            if can_go
                && egui_glass::GlassButton::new(t!("gallery.to_transcode"))
                    .show(ui)
                    .clicked()
            {
                if let Some(i) = self.gallery.selected {
                    if let Some(e) = self.gallery.entries.get(i) {
                        let src = std::path::Path::new(&dir)
                            .join(&e.name)
                            .display()
                            .to_string();
                        self.transcode.source = src;
                        self.refresh_probe();
                        self.page = crate::app::Page::Transcode;
                    }
                }
            }
        });
        ui.add_space(8.0);
        if self.gallery.entries.is_empty() {
            self.content_small(ui, t!("gallery.empty"));
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for i in 0..self.gallery.entries.len() {
                let e = &self.gallery.entries[i];
                let sel = self.gallery.selected == Some(i);
                let label = format!(
                    "{} {} ({} bytes, {})",
                    Self::marker(sel),
                    e.name,
                    e.size,
                    e.kind
                );
                if self.option_button(ui, sel, &label) {
                    self.gallery.selected = Some(i);
                }
            }
        });
    }
}

#[cfg(test)]
mod gallery_tests {
    use super::*;

    #[test]
    fn dir_follows_player_path() {
        assert_eq!(gallery_dir("player.toml"), ".");
        assert_eq!(gallery_dir("/Volumes/USB/player.toml"), "/Volumes/USB");
        assert_eq!(gallery_dir("sub/dir/player.toml"), "sub/dir");
    }

    #[test]
    fn kind_matrix() {
        for (name, kind) in [
            ("video.qois", "qois"),
            ("wall.qois", "qois"),
            ("frame0001.raw", "raw"),
            ("video.mp4", "mp4"),
            ("wall.mp4", "mp4"),
            ("video.hevc", "hevc"),
            ("video.ivf", "vp9"),
            ("video.h263", "h263"),
            ("video.m2v", "mpeg2v"),
            ("video.ogv", "theora"),
            ("player.toml", "other"),
            ("BOOTX64.EFI", "other"),
        ] {
            assert_eq!(media_kind(name), kind, "{name}");
        }
    }

    #[test]
    fn list_missing_dir_is_empty() {
        let entries = list_gallery("/nonexistent-dir-uefi-player-gallery");
        assert!(entries.is_empty());
    }
}
