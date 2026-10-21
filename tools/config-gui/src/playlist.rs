//! 播放列表 / 壁纸画廊列表编辑器（同 `impl App`，跨文件只为守 1000 行上限）。
//!
//! 轮子 verdict（见根 AGENTS.md「轮子优先」，搜过才写）：
//! - 搜过 `crates.io`：“egui drag reorder”只有 `egui_dnd`
//!  （lucasmerlin/hello_egui，MIT；SPEC  verdict 注 MIT OR Apache-2.0，
//!   ~100k+ 下载，持续维护；MSRV >= 1.80 < 本仓 1.92，可用；
//!   host-only `std` 轮子，固件 `no_std` 侧不受影响，与固件零依赖关系）。
//! - 对比过手写上/下按钮（零依赖，但只能单步挪；已保留为兜底，见每表 `Up/Down`
//!   按钮，拖拽不可用时仍可排序）与 `egui_extras` 表格排序器（要拖入整套
//!   `egui_extras` 表格件，与玻璃行不搭，且本仓未用该轮子，故不用）。
//! - 版本钉死说明：SPEC verdict 写 `egui_dnd 0.18`，但实测 `cargo tree`
//!   显示 0.18 拉 `egui 0.37`，与本仓 `egui 0.35` 双版本冲突（类型不通，
//!   编译不过）；`egui_dnd 0.16` 拉的正是 `egui 0.35`，同轮同用法，
//!   故用 `0.16`（caret），属 verdict 的版本 skew fallback（见 SPEC）。
//! - 路径只认图库目录字符串（`gallery_dir_of`，与 `gallery.rs` 同逻辑但不导入它：
//!   SPEC 禁止动 `gallery.rs/transcode.rs/codecs.rs`，三文件零改动）。
//! - 控件只用玻璃件（`GlassButton` 行/动作 + `GlassSlider` 循环档，留白分隔，
//!   无 `Button/TextEdit/ComboBox`；路径新增走 `rfd` 原生文件框，不手写输入框）。
//! - 重复条目允许：拖拽 id 用 `(tag, 下标)` 稳定包装（`RowItem`，见上游
//!   `index_as_id` 例），动画关零（id 在拖拽中不变），落点才写回列表。

use eframe::egui;
use rust_i18n::t;

use crate::app::App;
use crate::schema::{LoopMode, PlaylistItem};

/// 拖拽行包装（下标即 id，动画关零，落点写回；重复路径不打架）。
struct RowItem<T> {
    tag: &'static str,
    index: usize,
    item: T,
}

impl<T> egui_dnd::DragDropItem for RowItem<T> {
    fn id(&self) -> egui::Id {
        egui::Id::new((self.tag, self.index))
    }
}

/// 图库根目录（= `player.toml` 父目录；裸文件名按当前目录；与 `gallery.rs`
/// `gallery_dir` 同逻辑，SPEC 要求不导入该模块，故在此复刻三行）。
pub(crate) fn gallery_dir_of(player_path: &str) -> String {
    match std::path::Path::new(player_path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.display().to_string(),
        _ => String::from("."),
    }
}

/// 选中文件落盘用相对路径（能挂图库根下就挂相对串，固件侧直接按相对读；
/// 根外文件保留绝对串，不断新增）。
pub(crate) fn relativize(dir: &str, abs: &std::path::Path) -> String {
    match abs.strip_prefix(std::path::Path::new(dir)) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => abs.display().to_string(),
    }
}

impl App {
    /// 循环模式行（`GlassSlider` 0-2 拨 `LoopMode::ALL` 下标 + `GlassButton`
    /// 三颗单选镜像；复用加速/供片滑杆模式，无新件）。
    fn loop_slider_row(
        &mut self,
        ui: &mut egui::Ui,
        cur: LoopMode,
        mirror: &mut f32,
        hint_key: &str,
    ) -> LoopMode {
        let label = format!("{}: {}", cur.file_str(), crate::text::loop_desc(cur));
        egui_glass::GlassSlider::new(mirror, 0.0..=2.0)
            .text(label)
            .step_by(1.0)
            .style(if self.dark {
                egui_glass::GlassStyle::dark()
            } else {
                egui_glass::GlassStyle::regular()
            })
            .show(ui);
        let mut next = LoopMode::ALL[mirror.round().clamp(0.0, 2.0) as usize];
        ui.horizontal_wrapped(|ui| {
            for v in LoopMode::ALL {
                if self.option_button(ui, next == v, v.file_str()) {
                    next = v;
                }
            }
        });
        ui.small(t!(hint_key));
        next
    }

    /// 播放列表循环行（读 `cfg.playlist`，写回 + 镜像同步）。
    pub(crate) fn playlist_loop_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("playlist.loop"));
        let cur = self.cfg.playlist.loop_mode;
        // 借用拆开：滑杆拿 `playlist_loop_f` 可变借，文本先算好（播放页同式）。
        let mut mirror = self.playlist_loop_f;
        let next = self.loop_slider_row(ui, cur, &mut mirror, "playlist.loop_hint");
        self.cfg.playlist.loop_mode = next;
        self.playlist_loop_f = next.index() as f32;
    }

    /// 壁纸画廊循环行（读 `cfg.wallpaper`，写回 + 镜像同步）。
    pub(crate) fn wallpaper_loop_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("wallpaper.loop"));
        let cur = self.cfg.wallpaper.loop_mode;
        let mut mirror = self.wp_loop_f;
        let next = self.loop_slider_row(ui, cur, &mut mirror, "wallpaper.loop_hint");
        self.cfg.wallpaper.loop_mode = next;
        self.wp_loop_f = next.index() as f32;
    }

    /// 通用列表编辑器（拖拽主路 + 上/下按钮兜底 + `rfd` 新增 + 删除选中）。
    /// `items`/`sel`/`tag` 按播放列表或壁纸画廊传入；调用方负责外层标题。
    fn list_editor(
        &mut self,
        ui: &mut egui::Ui,
        items: &mut Vec<PlaylistItem>,
        sel: &mut Option<usize>,
        tag: &'static str,
        count_key: &str,
        empty_key: &str,
    ) {
        let dark = self.dark;
        let gallery_dir = gallery_dir_of(&self.path);
        ui.small(t!(count_key, n = items.len()));
        if items.is_empty() {
            ui.small(t!(empty_key));
        }
        // 拖拽主路（下标即 id，动画关零，落点才写回，重复路径不打架）。
        let mut next_sel = *sel;
        let selected_path = sel.and_then(|i| items.get(i).map(|it| it.path.clone()));
        let response = egui_dnd::dnd(ui, egui::Id::new((tag, "list")))
            .with_animation_time(0.0)
            .show(
                items.iter_mut().enumerate().map(|(i, item)| RowItem {
                    tag,
                    index: i,
                    item,
                }),
                |ui, wrap, handle, _state| {
                    let i = wrap.index;
                    ui.horizontal(|ui| {
                        // 拖柄只管拖（≡ 文本，无按钮语义），点选走右侧玻璃钮，
                        // 点击与拖拽不打架。
                        handle.ui(ui, |ui| {
                            ui.label("≡");
                        });
                        let selected = next_sel == Some(i);
                        let mut b = egui_glass::GlassButton::new(format!(
                            "{} #{}",
                            Self::marker(selected),
                            i + 1
                        ));
                        if selected {
                            b = b.text_color(Self::accent(dark));
                        }
                        if b.show(ui).clicked() {
                            next_sel = if selected { None } else { Some(i) };
                        }
                        ui.monospace(wrap.item.path.as_str());
                    });
                },
            );
        // 落点写回（`update_vec` 按 `DragUpdate{from,to}` 移位）。
        if response.is_drag_finished() {
            response.update_vec(items);
            // 选中跟人走（记路径重找；删行后悬空由 `sync_widgets` 钳住）。
            next_sel = selected_path
                .as_ref()
                .and_then(|p| items.iter().position(|it| &it.path == p));
        }
        *sel = next_sel;
        ui.add_space(4.0);
        // 动作行（全玻璃；上/下是拖拽的零依赖兜底，SPEC fallback）。
        ui.horizontal_wrapped(|ui| {
            if egui_glass::GlassButton::new(t!("playlist.add"))
                .show(ui)
                .clicked()
            {
                if let Some(f) = rfd::FileDialog::new()
                    .set_directory(gallery_dir.as_str())
                    .pick_file()
                {
                    let rel = relativize(gallery_dir.as_str(), &f);
                    items.push(PlaylistItem { path: rel.clone() });
                    *sel = Some(items.len() - 1);
                    self.status = t!("playlist.added", v = rel).into_owned();
                }
            }
            if egui_glass::GlassButton::new(t!("playlist.remove"))
                .show(ui)
                .clicked()
            {
                if let Some(i) = *sel {
                    if i < items.len() {
                        let gone = items.remove(i).path;
                        *sel = None;
                        self.status = t!("playlist.removed", v = gone).into_owned();
                    }
                }
            }
            if egui_glass::GlassButton::new(t!("playlist.up"))
                .show(ui)
                .clicked()
            {
                if let Some(i) = *sel {
                    if i > 0 && i < items.len() {
                        items.swap(i - 1, i);
                        *sel = Some(i - 1);
                    }
                }
            }
            if egui_glass::GlassButton::new(t!("playlist.down"))
                .show(ui)
                .clicked()
            {
                if let Some(i) = *sel {
                    if i + 1 < items.len() {
                        items.swap(i, i + 1);
                        *sel = Some(i + 1);
                    }
                }
            }
        });
    }

    /// 播放列表编辑器（循环行 + 列表；`fps`/`feed` 全局默认已搬解码页顶）。
    pub(crate) fn playlist_list_editor(&mut self, ui: &mut egui::Ui) {
        self.playlist_loop_row(ui);
        ui.add_space(8.0);
        // 借用拆开：编辑器要 `&mut items` + `&mut sel` + `&mut self.status`，
        // 先把不相干字段拷出，行内只碰这三处（借用检查器按字段拆分可过）。
        let mut items = std::mem::take(&mut self.cfg.playlist.items);
        let mut sel = self.playlist_sel;
        self.list_editor(
            ui,
            &mut items,
            &mut sel,
            "playlist",
            "playlist.count",
            "playlist.empty",
        );
        self.cfg.playlist.items = items;
        self.playlist_sel = sel;
    }

    /// 壁纸画廊列表（仅列表；循环行由调用方另摆，顺序见 `[wallpaper]` 表）。
    pub(crate) fn wallpaper_items_editor(&mut self, ui: &mut egui::Ui) {
        let mut items = std::mem::take(&mut self.cfg.wallpaper.items);
        let mut sel = self.wallpaper_sel;
        self.list_editor(
            ui,
            &mut items,
            &mut sel,
            "wallpaper",
            "wallpaper.count",
            "wallpaper.empty",
        );
        self.cfg.wallpaper.items = items;
        self.wallpaper_sel = sel;
    }

    /// 播放列表页（循环 + 画廊列表；`fps`/`feed` 全局默认见解码页顶）。
    pub(crate) fn playlist_page(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("playlist.heading"));
        ui.small(t!("playlist.hint"));
        ui.add_space(4.0);
        self.playlist_list_editor(ui);
    }
}

#[cfg(test)]
mod playlist_tests {
    use super::*;
    use crate::schema::{FileConfig, LoopMode};

    #[test]
    fn dir_follows_player_path() {
        assert_eq!(gallery_dir_of("player.toml"), ".");
        assert_eq!(gallery_dir_of("/Volumes/USB/player.toml"), "/Volumes/USB");
        assert_eq!(gallery_dir_of("sub/dir/player.toml"), "sub/dir");
    }

    #[test]
    fn relativize_keeps_inside_relative() {
        let abs = std::path::Path::new("1080p/video.mp4");
        // 相对根 + 相对文件：`strip_prefix` 直挂相对串。
        assert_eq!(relativize(".", abs), "1080p/video.mp4");
        let outside = std::path::Path::new("/tmp/clip.mp4");
        assert_eq!(relativize(".", outside), "/tmp/clip.mp4");
    }

    #[test]
    fn row_id_stable_per_index() {
        use egui_dnd::DragDropItem;
        let a = RowItem {
            tag: "playlist",
            index: 2,
            item: 0u8,
        };
        let b = RowItem {
            tag: "playlist",
            index: 2,
            item: 0u8,
        };
        let c = RowItem {
            tag: "playlist",
            index: 3,
            item: 0u8,
        };
        // 同下标同 id（重复路径不打架靠下标区分），异下标异 id。
        assert_eq!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        // 播放列表与壁纸画廊命名空间隔离。
        let w = RowItem {
            tag: "wallpaper",
            index: 2,
            item: 0u8,
        };
        assert_ne!(a.id(), w.id());
    }

    /// 播放列表多条目回写（`playlist-list-loop`：3 条目 × `loop_list`，
    /// 落盘 `[[playlist.items]]` + `loop` 键齐全；顺序即播放序）。
    /// 黑盒纯 TOML（无 display），与 `rules.rs` 矩阵同跑。
    #[test]
    fn playlist_list_loop() {
        let mut cfg = FileConfig::default();
        cfg.playlist.loop_mode = LoopMode::LoopList;
        cfg.playlist.items = vec![
            PlaylistItem {
                path: String::from("1080p/video.mp4"),
            },
            PlaylistItem {
                path: String::from("1080p/video.hevc"),
            },
            PlaylistItem {
                path: String::from("1080p/video.qois"),
            },
        ];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"loop_list\""),
            "loop key missing in {text}"
        );
        assert_eq!(text.matches("[[playlist.items]]").count(), 3);
        for p in ["1080p/video.mp4", "1080p/video.hevc", "1080p/video.qois"] {
            assert!(text.contains(p), "{p} missing in {text}");
        }
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.playlist.loop_mode, LoopMode::LoopList);
        assert_eq!(back.playlist.items.len(), 3);
        assert_eq!(back.playlist.items[0].path, "1080p/video.mp4");
        assert_eq!(back.playlist.items[1].path, "1080p/video.hevc");
        assert_eq!(back.playlist.items[2].path, "1080p/video.qois");
        assert_eq!(back, cfg);
    }

    /// 播放列表单条循环（`playlist-loop-single`：单条目 × `loop_single`，
    /// 当前条目重播，落盘同式）。
    #[test]
    fn playlist_loop_single() {
        let mut cfg = FileConfig::default();
        cfg.playlist.loop_mode = LoopMode::LoopSingle;
        cfg.playlist.items = vec![PlaylistItem {
            path: String::from("1080p/video.mp4"),
        }];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"loop_single\""),
            "loop key missing in {text}"
        );
        assert_eq!(text.matches("[[playlist.items]]").count(), 1);
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.playlist.loop_mode, LoopMode::LoopSingle);
        assert_eq!(back.playlist.items.len(), 1);
        assert_eq!(back, cfg);
    }

    /// 播放列表播完即停（`playlist-once`：2 条目 × `once`，播完回菜单；
    /// 空列表同式回写，固件侧空列表 = `Err`，无静默菜单）。
    #[test]
    fn playlist_once() {
        let mut cfg = FileConfig::default();
        cfg.playlist.loop_mode = LoopMode::Once;
        cfg.playlist.items = vec![
            PlaylistItem {
                path: String::from("1080p/video.mp4"),
            },
            PlaylistItem {
                path: String::from("1080p/video.qois"),
            },
        ];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"once\""),
            "loop key missing in {text}"
        );
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.playlist.loop_mode, LoopMode::Once);
        assert_eq!(back.playlist.items.len(), 2);
        assert_eq!(back, cfg);
        // 空列表同式回写（条目零个，不断解析；固件侧空列表 = `Err`）。
        let empty = FileConfig::default();
        let etext = toml::to_string(&empty).unwrap();
        assert!(!etext.contains("[[playlist.items]]"));
        let eback: FileConfig = toml::from_str(&etext).unwrap();
        assert!(eback.playlist.items.is_empty());
        assert_eq!(eback, empty);
    }

    /// 壁纸画廊列表回写（`wallpaper-gallery-list`：2 条目 × `loop_list`，
    /// 落盘 `[[wallpaper.items]]` + `loop` 键齐全；`enable/fps/preload` 同表）。
    #[test]
    fn wallpaper_gallery_list() {
        let mut cfg = FileConfig::default();
        cfg.wallpaper.enable = true;
        cfg.wallpaper.loop_mode = LoopMode::LoopList;
        cfg.wallpaper.preload = 8;
        cfg.wallpaper.items = vec![
            PlaylistItem {
                path: String::from("wallpaper/wall.mp4"),
            },
            PlaylistItem {
                path: String::from("wallpaper/wall.qois"),
            },
        ];
        let text = toml::to_string(&cfg).unwrap();
        assert!(
            text.contains("loop = \"loop_list\""),
            "loop key missing in {text}"
        );
        assert_eq!(text.matches("[[wallpaper.items]]").count(), 2);
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert!(back.wallpaper.enable);
        assert_eq!(back.wallpaper.loop_mode, LoopMode::LoopList);
        assert_eq!(back.wallpaper.items.len(), 2);
        assert_eq!(back.wallpaper.items[0].path, "wallpaper/wall.mp4");
        assert_eq!(back.wallpaper.preload, 8);
        assert_eq!(back, cfg);
    }
}
