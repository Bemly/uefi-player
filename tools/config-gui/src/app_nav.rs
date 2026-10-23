//! 导航与动作（同 `impl App`，`app.rs` 触达即拆：跨文件只为守 1000 行上限，行为零改）。
//!
//! - `do_action`：rfd 收果 + 打开/保存/另存/重置/主题/语言六动作。
//! - `action_buttons`/`icon_btn`：右下悬浮图标胶囊（`GlassButton` 空文本圆钮 + 像素贴图）。
//! - `nav_button`/`option_button`/`switch_row`/`decode_tab_bar`：左侧导航与单选开关。
//! - `page_backdrop`/`cover_shifted`：中央提示行与背景右移几何（纯数学，可单测）。

use eframe::egui;
use rust_i18n::t;

use crate::app::{Action, App, DecodeTab, Page};
use crate::schema::*;
use crate::text::decode_tab_name;

impl App {
    pub(crate) fn do_action(&mut self, ctx: &egui::Context, act: Action) {
        // rfd 收果（每帧轮询：后台线程框选完落袋/取消；见 `file_pick`）。
        if let Some(f) = self.open_pick.take().flatten() {
            self.load_path(f.display().to_string());
        }
        if let Some(f) = self.save_pick.take().flatten() {
            let p = f.display().to_string();
            self.path = p.clone();
            self.save_to(&p);
        }
        match act {
            Action::None => {}
            Action::Open => {
                // 后台线程跑阻塞框（winit 重入 abort 血泪，见 `file_pick`）；
                // 框开着时不再起新框。
                if !self.open_pick.pending() {
                    self.open_pick.open(|| {
                        rfd::FileDialog::new()
                            .add_filter("TOML", &["toml"])
                            .pick_file()
                    });
                }
            }
            Action::Save => {
                let p = self.path.clone();
                self.save_to(&p);
            }
            Action::SaveAs => {
                if !self.save_pick.pending() {
                    self.save_pick.open(|| {
                        rfd::FileDialog::new()
                            .add_filter("TOML", &["toml"])
                            .set_file_name("player.toml")
                            .save_file()
                    });
                }
            }
            Action::Reset => {
                self.cfg = FileConfig::default();
                // P5 `media_dir` 缺省 `""`：`cfg` 回默认即覆盖（薄代理，无独立缓冲可清）。
                self.sync_widgets();
                self.tune_at_load = self.cfg.render.tune;
                self.status = t!("status.reset").into_owned();
            }
            Action::ToggleTheme => {
                self.dark = !self.dark;
                // 偏好与 visuals 同步换槽（见 `App::new` 注释，不跟系统）。
                ctx.set_theme(if self.dark {
                    egui::ThemePreference::Dark
                } else {
                    egui::ThemePreference::Light
                });
                ctx.set_visuals(if self.dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                self.status = t!(
                    "status.theme",
                    theme = if self.dark {
                        t!("theme.dark")
                    } else {
                        t!("theme.light")
                    },
                )
                .into_owned();
            }
            Action::ToggleLang => {
                self.zh = !self.zh;
                rust_i18n::set_locale(if self.zh { "zh-CN" } else { "en" });
                // 标题栏跟语言走：英文 UEFI Player / 中文 UEFI播放器。
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
                self.status = t!("status.ready").into_owned();
            }
        }
    }

    /// 右下悬浮动作条：圆形像素图标按钮（用户拍板：文字改图标）。
    /// 轮子结论：`GlassButton` 只吃文本，库内无图标件；故空文本圆钮（`icon()`
    /// 方垫 + `accessible_name` + 悬安示保障）+ MIT 单色像素位图
    /// （`assets/icons/`，`NEAREST` 20pt 直显 + 主题色 tint，见该目录 README）。
    /// 布局（用户拍板，像图二）：5 图标连体胶囊（打开→保存→另存为→重置→语言），
    /// 明暗开关单独成圆、隔 8px 摆最边上。
    pub(crate) fn action_buttons(&mut self, ui: &mut egui::Ui) -> Action {
        let mut act = Action::None;

        // 图标黑白跟随字体颜色（用户拍板）：暗色白 / 浅色黑与描边字同口径。
        let col = crate::theme::fg(self.dark);
        ui.horizontal(|ui| {
            egui_glass::GlassToolbar::new(self.bar_style()).show(ui, |ui| {
                // 连体胶囊：打开 → 保存 → 另存为 → 重置 → 语言（44 圆）。
                if Self::icon_btn(
                    ui,
                    t!("toolbar.open").into_owned(),
                    &self.icons.open,
                    col,
                    44.0,
                )
                .clicked()
                {
                    act = Action::Open;
                }
                if Self::icon_btn(
                    ui,
                    t!("toolbar.save").into_owned(),
                    &self.icons.save,
                    col,
                    44.0,
                )
                .clicked()
                {
                    act = Action::Save;
                }
                if Self::icon_btn(
                    ui,
                    t!("toolbar.save_as").into_owned(),
                    &self.icons.save_as,
                    col,
                    44.0,
                )
                .clicked()
                {
                    act = Action::SaveAs;
                }
                if Self::icon_btn(
                    ui,
                    t!("toolbar.reset").into_owned(),
                    &self.icons.reset,
                    col,
                    44.0,
                )
                .clicked()
                {
                    act = Action::Reset;
                }
                let lang = if self.zh { "EN" } else { "中文" }.to_owned();
                if Self::icon_btn(ui, lang, &self.icons.lang, col, 44.0).clicked() {
                    act = Action::ToggleLang;
                }
            });
            ui.add_space(8.0);
            // 明暗开关单独成圆（56 圆与胶囊等高）：深色下给亮框满月（切浅色），
            // 浅色下给暗框新月（切深色）；图标即目的地，见 icons README。
            let (theme_tip, theme_icon) = if self.dark {
                (t!("toolbar.to_light").into_owned(), &self.icons.theme_light)
            } else {
                (t!("toolbar.to_dark").into_owned(), &self.icons.theme_dark)
            };
            if Self::icon_btn(ui, theme_tip, theme_icon, col, 56.0).clicked() {
                act = Action::ToggleTheme;
            }
        });
        act
    }

    /// 空文本圆形图标钮：`GlassButton::icon` 玻璃圆底 + 像素贴图。
    /// `diam` 是圆直径：胶囊里用 44（库内件默认最小边），独立圆用 56
    /// （= 44 按钮 + 工具条上下内边距 6×2，与连体胶囊等高，用户拍板）。
    /// 贴图 20pt `NEAREST` 居中直显（硬边缘，不模糊）。
    fn icon_btn(
        ui: &mut egui::Ui,
        label: String,
        icon: &egui::TextureHandle,
        col: egui::Color32,
        diam: f32,
    ) -> egui::Response {
        let r = egui_glass::GlassButton::new("")
            .icon()
            .min_size(egui::vec2(diam, diam))
            .accessible_name(label.clone())
            .show(ui)
            .on_hover_text(label);
        if ui.is_rect_visible(r.rect) {
            let side = 20.0;
            ui.painter().image(
                icon.id(),
                egui::Rect::from_center_size(r.rect.center(), egui::vec2(side, side)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                col,
            );
        }
        r
    }

    /// 左侧导航：全宽玻璃按钮，选中态描蓝 + ●。
    pub(crate) fn nav_button(&mut self, ui: &mut egui::Ui, page: Page, label: String) {
        let selected = self.page == page;
        let mut b = egui_glass::GlassButton::new(format!("{} {}", Self::marker(selected), label))
            .min_size(egui::vec2(ui.available_width(), 30.0));
        if selected {
            b = b.text_color(Self::accent(self.dark));
        }
        if b.show(ui).clicked() {
            self.page = page;
        }
    }

    /// 单选开关：玻璃按钮 + ●○ 标记，点中返回 true（调用方负责赋值）。
    pub(crate) fn option_button(&self, ui: &mut egui::Ui, selected: bool, name: &str) -> bool {
        let mut b = egui_glass::GlassButton::new(format!("{} {}", Self::marker(selected), name));
        if selected {
            b = b.text_color(Self::accent(self.dark));
        }
        b.show(ui).clicked()
    }

    /// 解码页顶部固定 tab 条（滚动区之外调用，不随内容滚动；`Global` = 全局默认帧率 + 供片）。
    /// 轮子结论（搜过 `crates.io`，2026-10-21）：`egui_tabs` 锁 `egui ^0.29`（本仓 0.35，
    /// 并存两大版本类型不通）；`facett-tabbar` 是个人小框架（总下载三位数），拖整套
    /// `facett-core` 主题栈且非玻璃风；`egui_dock` 是 5000 行 docking 系统，
    /// 自带动拖拽且原生渲染——三个都不用。故库内件拼：`GlassToolbar` 胶囊当条 +
    /// 9 个 `GlassButton`（`spacing(2.0)` 连成一体，不断尾横滚）。
    /// `id_salt` 必加：横竖两个 `ScrollArea` 的自动 ID 会撞车（曾报
    /// `First/Second use of ScrollArea ID` 红字），显式盐隔离。
    pub(crate) fn decode_tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        egui::ScrollArea::horizontal()
            .id_salt("decode_tabs")
            .show(ui, |ui| {
                egui_glass::GlassToolbar::new(self.bar_style())
                    .spacing(2.0)
                    .show(ui, |ui| {
                        for tab in DecodeTab::ALL {
                            if self.option_button(ui, self.decode_tab == tab, &decode_tab_name(tab))
                            {
                                self.decode_tab = tab;
                            }
                        }
                    });
            });
        ui.add_space(4.0);
    }

    /// 开/关行（两颗玻璃按钮）：返回用户点选后的值，调用方写回。
    /// 纯为了借用简单（直接在循环里拿 `&mut` 字段会和 `option_button` 的 `&self` 打架）。
    pub(crate) fn switch_row(&mut self, ui: &mut egui::Ui, cur: bool, on: &str, off: &str) -> bool {
        let mut next = cur;
        ui.horizontal(|ui| {
            if self.option_button(ui, cur, on) {
                next = true;
            }
            if self.option_button(ui, !cur, off) {
                next = false;
            }
        });
        next
    }

    /// 中央透明区头的提示行：描边字，不进折射源（折射源只有根部
    /// `live.run` 的背景图，纯绘制不占布局）。摘要行只放左侧导航卡，
    /// 不在这里重复占右侧空间。
    pub(crate) fn page_backdrop(&self, page: &mut egui::Ui) {
        page.add_space(4.0);
        // 纯白/纯黑（用户拍板：不用 `.weak()` 灰；`outlined` 本来也会剥色，此处明示）。
        self.content_small(page, egui::RichText::new(t!("sum.hint")).small());
        page.add_space(4.0);
    }

    /// cover 矩形（中心右移 `shift_x`；溢出允许，调用方限裁剪）。
    /// 背景脸在片源偏左，居中 cover 会把它压进 216 宽导航区
    /// （糊脸 + 清晰脸看着像重影）；右移让脸滑进中央区。纯数学，可单测。
    /// 硬约束：平移后仍必须盖住全屏（否则露出的 `clear` 色就是左白边，
    /// 2026-10-22 故障）。`sized.w >= sw + 2*|shift|` 即得。
    pub(crate) fn cover_shifted(screen: egui::Rect, iw: f32, ih: f32, shift_x: f32) -> egui::Rect {
        let base = (screen.width() / iw).max(screen.height() / ih);
        let need = (screen.width() + 2.0 * shift_x.abs()) / iw;
        let scale = base.max(need);
        let sized = egui::vec2(iw * scale, ih * scale);
        egui::Rect::from_center_size(
            egui::pos2(screen.center().x + shift_x, screen.center().y),
            sized,
        )
    }
}
