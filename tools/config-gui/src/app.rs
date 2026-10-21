//! 应用壳：`App` 状态 + `Page` 导航 + 除 `page_content` 外的全部方法 + `eframe::App` 实现。
//! 页面内容见 `pages`（同 `impl App`，跨文件只为守 1000 行上限）。

use eframe::egui;
use rust_i18n::t;

use crate::make_backdrop;
use crate::rules::*;
use crate::schema::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    File,
    Accel,
    Player,
    Wallpaper,
    Boot,
    Summary,
}

pub(crate) enum Action {
    None,
    Open,
    Save,
    SaveAs,
    Reset,
    ToggleTheme,
    ToggleLang,
}

pub(crate) struct App {
    pub(crate) path: String,
    pub(crate) cfg: FileConfig,
    pub(crate) status: String,
    pub(crate) page: Page,
    /// 播放帧率滑杆镜像：0 = auto，1-240 = 限速，241 = max。
    /// `GlassSlider` 只要 `f32`，每帧写回 `cfg.play.fps`。
    pub(crate) play_f: f32,
    /// 加速档位滑杆镜像：0-8 对应 `AccelLevel::ALL` 下标。
    pub(crate) accel_f: f32,
    /// 壁纸滑杆镜像：fps 1-30，frames 0-120（0 = 全播），preload 0-32（0 = 不封顶）。
    /// 库内滑杆只吃 `f32`，整数档用 round 镜像（见 Accel 页 `*_f` 字段）。
    pub(crate) wp_fps_f: f32,
    pub(crate) wp_frames_f: f32,
    pub(crate) wp_preload_f: f32,
    /// 微调滑杆镜像：frames 1-120，tile_rows 8-512（步进 8），vsync_fps 15-240，
    /// buffers 0-16（0 = 直写，步进 1）。
    pub(crate) tune_frames_f: f32,
    pub(crate) tune_tile_f: f32,
    pub(crate) tune_vsync_fps_f: f32,
    pub(crate) tune_buffers_f: f32,
    /// 存盘时对比：tune 变了就清空全部实测记忆（bench 条件变了，旧结论作废，
    /// 下次启动重测；否则改 tune 永不生效——记忆命中直接跳过矩阵）。
    pub(crate) tune_at_load: RenderTune,
    pub(crate) glass: bool,
    /// 深色模式：egui visuals + 纯黑 backdrop + 深色玻璃预设三处联动。
    pub(crate) dark: bool,
    /// 中文模式：`rust_i18n::set_locale` 全局切换，文案全走 `t!`。
    pub(crate) zh: bool,
    /// 存一份 render state，深浅切换时重传 backdrop 用（`RenderState: Clone`）。
    pub(crate) rs: Option<egui_wgpu::RenderState>,
    /// 实时背景：页面跑两遍（可见 + 离屏折射源），开关浮在上层。无 GPU 时为 None。
    pub(crate) live: Option<egui_glass::LiveBackdrop>,
}

impl App {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 缝合像素 10px 简中全量优先（西文缺字形会 fallback 到默认字体）。
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "fusion".to_owned(),
            egui::FontData::from_static(include_bytes!(
                "../assets/fusion-pixel-10px-proportional-zh_hans.ttf"
            ))
            .into(),
        );
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            fam.insert(0, "fusion".to_owned());
        }
        if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            fam.insert(0, "fusion".to_owned());
        }
        // 双胞胎上下文用同一套字体（含 cjk），离屏排版才和可见一致。
        let live_fonts = fonts.clone();
        cc.egui_ctx.set_fonts(fonts);

        // 默认英文 + 浅色（白底），与初始 backdrop 一致。
        // 窗口标题跟语言走（中英切换时见 ToggleLang），首帧即按当前 locale 下发。
        rust_i18n::set_locale("en");
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut glass = false;
        let mut rs = None;
        let mut live = None;
        if let Some(r) = cc.wgpu_render_state.as_ref() {
            // MSAA 数与 NativeOptions 一致（默认不开 MSAA，传 1）。
            egui_glass::init(r, 1);
            // 顺序按官方要求：init → live → set_backdrop（静态底只当兜底，
            // live 每帧都跑时实际采的是离屏页面）。
            live = Some(egui_glass::LiveBackdrop::new(r, Some(live_fonts)));
            egui_glass::set_backdrop(&cc.egui_ctx, r, &make_backdrop(false));
            glass = true;
            rs = Some(r.clone());
        }
        let mut app = Self {
            path: "player.toml".to_owned(),
            cfg: FileConfig::default(),
            status: t!("status.ready").into_owned(),
            page: Page::File,
            play_f: 0.0,
            accel_f: AccelLevel::Auto.index() as f32,
            wp_fps_f: 15.0,
            wp_frames_f: 0.0,
            wp_preload_f: default_wallpaper_preload() as f32,
            tune_frames_f: default_frames() as f32,
            tune_tile_f: default_tile_rows() as f32,
            tune_vsync_fps_f: default_vsync_fps() as f32,
            tune_buffers_f: default_buffers() as f32,
            tune_at_load: RenderTune::default(),
            glass,
            dark: false,
            zh: false,
            rs,
            live,
        };
        // 启动时若工作目录下已有 player.toml，直接载入。
        if std::path::Path::new(&app.path).exists() {
            let p = app.path.clone();
            app.load_path(p);
        }
        app
    }

    pub(crate) fn sync_widgets(&mut self) {
        self.play_f = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => 0.0,
            FpsPolicy::Max => 241.0,
            FpsPolicy::Capped(n) => n as f32,
        };
        self.wp_fps_f = match parse_fps(&self.cfg.wallpaper.fps) {
            FpsPolicy::Auto => 0.0,
            FpsPolicy::Max => 241.0,
            FpsPolicy::Capped(n) => n.min(240).max(1) as f32,
        };
        self.wp_frames_f = self.cfg.wallpaper.frames.min(120) as f32;
        self.wp_preload_f = self.cfg.wallpaper.preload.min(32) as f32;
        self.tune_frames_f = self.cfg.render.tune.frames.clamp(1, 120) as f32;
        self.tune_tile_f = self.cfg.render.tune.tile_rows.clamp(8, 512) as f32;
        self.tune_vsync_fps_f = self.cfg.render.tune.vsync_fps.clamp(15, 240) as f32;
        self.tune_buffers_f = self.cfg.render.tune.buffers.min(16) as f32;
    }

    /// 滑杆/摘要共用的档位文本。
    pub(crate) fn play_text(&self) -> String {
        let base = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => t!("play.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n),
        };
        format!("{} {}", base, self.cfg.play.format.file_str())
    }

    /// 壁纸帧率滑杆文本（三态与播放同构；auto 跟 vsync，详见固件 `effective_wallpaper_fps`）。
    pub(crate) fn wp_fps_text(&self) -> String {
        match parse_fps(&self.cfg.wallpaper.fps) {
            FpsPolicy::Auto => t!("wallpaper.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n.min(240).max(1)),
        }
    }

    /// 片源格式行（两颗玻璃按钮，qois/raw 二选一，壁纸/播放共用形状）。
    pub(crate) fn format_row(&mut self, ui: &mut egui::Ui, cur: MediaFormat) -> MediaFormat {
        let mut next = cur;
        ui.horizontal(|ui| {
            if self.option_button(ui, cur == MediaFormat::Qois, &t!("format.qois")) {
                next = MediaFormat::Qois;
            }
            if self.option_button(ui, cur == MediaFormat::Raw, &t!("format.raw")) {
                next = MediaFormat::Raw;
            }
        });
        next
    }

    /// 解码开关行（三颗玻璃按钮，auto/scalar/max 三选一；hevc/vp9/h263 共用形状，
    /// 与同页 workers 三态同式——单选用 GlassButton，不用滑杆，故无 `*_f` 镜像）。
    pub(crate) fn decode_row(&mut self, ui: &mut egui::Ui, cur: DecodeSimd) -> DecodeSimd {
        let mut next = cur;
        ui.horizontal(|ui| {
            for v in DecodeSimd::ALL {
                if self.option_button(ui, cur == v, v.file_str()) {
                    next = v;
                }
            }
        });
        next
    }

    /// 实测行：有值报数，无值提示切 auto 下次启动测。
    /// `&self`：背景纸（`page_backdrop`）只读重排也能调，不写状态。
    pub(crate) fn measured_line(&self) -> String {
        if self.cfg.play.measured_max_fps > 0 {
            t!("play.measured", v = self.cfg.play.measured_max_fps).into_owned()
        } else {
            t!("play.unmeasured").into_owned()
        }
    }

    /// half 实测行：有值报预算，无值提示下次启动现测取半。
    pub(crate) fn half_line(&self) -> String {
        if self.cfg.render.measured_vsync_fps > 0 {
            t!(
                "vsync.half_measured",
                v = self.cfg.render.measured_vsync_fps
            )
            .into_owned()
        } else {
            t!("vsync.half_unmeasured").into_owned()
        }
    }

    /// 渲染矩阵记忆行：全未测提示切 auto，否则报记忆值 + 校准时机型核数。
    pub(crate) fn render_measured_line(&self) -> String {
        let m = &self.cfg.render;
        if m.measured_render == RenderMode::Auto
            && m.measured_workers == WorkersMode::Auto
            && m.measured_dist == DistMode::Auto
            && m.measured_vsync == VsyncMode::Auto
            && m.measured_cores == 0
        {
            t!("render.unmeasured").into_owned()
        } else {
            t!(
                "render.measured",
                v = format!(
                    "{}/{}/{}/{}, {} cores",
                    m.measured_render.file_str(),
                    m.measured_workers.file_str(),
                    m.measured_dist.file_str(),
                    m.measured_vsync.file_str(),
                    m.measured_cores
                )
            )
            .into_owned()
        }
    }

    pub(crate) fn load_path(&mut self, path: String) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(cfg) => {
                    // 缺 `[schedule]` 表即老文件：`[render]` 旧键迁过来（见 coerce）。
                    let has_schedule = toml::from_str::<toml::Table>(&text)
                        .map(|t| t.contains_key("schedule"))
                        .unwrap_or(false);
                    let cfg = coerce_loaded(cfg, has_schedule);
                    let msg = t!(
                        "status.loaded",
                        path = path.clone(),
                        accel = cfg.accel.level.file_str(),
                        render = cfg.render.mode.file_str()
                    )
                    .into_owned();
                    self.cfg = cfg;
                    self.sync_widgets();
                    self.tune_at_load = self.cfg.render.tune;
                    self.status = msg;
                    self.path = path;
                }
                Err(e) => {
                    self.status =
                        t!("status.parse_error", path = path, err = e.to_string()).into_owned();
                }
            },
            Err(e) => {
                self.status =
                    t!("status.read_error", path = path, err = e.to_string()).into_owned();
            }
        }
    }

    pub(crate) fn save_to(&mut self, path: &str) {
        clamp_tune(&mut self.cfg.render.tune);
        // tune 变了 = bench 条件变了：旧记忆作废，下次启动重测。
        // 不做这个，改 tune 会被“有记忆快启”吞掉，永远不生效。
        if self.cfg.render.tune != self.tune_at_load {
            self.cfg.render.measured_render = RenderMode::Auto;
            self.cfg.render.measured_workers = WorkersMode::Auto;
            self.cfg.render.measured_dist = DistMode::Auto;
            self.cfg.render.measured_vsync = VsyncMode::Auto;
            self.cfg.render.measured_vsync_fps = 0;
            self.cfg.render.measured_cores = 0;
            self.cfg.play.measured_max_fps = 0;
        }
        self.tune_at_load = self.cfg.render.tune;
        match toml::to_string(&self.cfg) {
            Ok(text) => match std::fs::write(path, text) {
                Ok(()) => {
                    self.status = t!("status.saved", path = path).into_owned();
                }
                Err(e) => {
                    self.status =
                        t!("status.write_error", path = path, err = e.to_string()).into_owned();
                }
            },
            Err(e) => {
                self.status = t!("status.serialize_error", err = e.to_string()).into_owned();
            }
        }
    }

    /// 选中态强调色（深浅主题各一，保证在玻璃上可读）。
    pub(crate) fn accent(dark: bool) -> egui::Color32 {
        if dark {
            egui::Color32::LIGHT_BLUE
        } else {
            egui::Color32::from_rgb(0, 90, 200)
        }
    }

    /// 单选开关的选中标记（● ○ 全量字体自带，比例字体下也有字形）。
    pub(crate) fn marker(selected: bool) -> &'static str {
        if selected {
            "●"
        } else {
            "○"
        }
    }

    /// 工具栏/状态条的玻璃材质：深色配深色预设，浅色配默认。
    pub(crate) fn bar_style(&self) -> egui_glass::GlassStyle {
        if self.dark {
            egui_glass::GlassStyle::dark()
        } else {
            egui_glass::GlassStyle::regular()
        }
    }

    pub(crate) fn do_action(&mut self, ctx: &egui::Context, act: Action) {
        match act {
            Action::None => {}
            Action::Open => {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .pick_file()
                {
                    self.load_path(f.display().to_string());
                }
            }
            Action::Save => {
                let p = self.path.clone();
                self.save_to(&p);
            }
            Action::SaveAs => {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .set_file_name("player.toml")
                    .save_file()
                {
                    let p = f.display().to_string();
                    self.path = p.clone();
                    self.save_to(&p);
                }
            }
            Action::Reset => {
                self.cfg = FileConfig::default();
                self.sync_widgets();
                self.tune_at_load = self.cfg.render.tune;
                self.status = t!("status.reset").into_owned();
            }
            Action::ToggleTheme => {
                self.dark = !self.dark;
                ctx.set_visuals(if self.dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                if let Some(rs) = self.rs.as_ref() {
                    egui_glass::set_backdrop(ctx, rs, &make_backdrop(self.dark));
                }
                self.status = t!(
                    "status.theme",
                    theme = if self.dark {
                        t!("theme.dark")
                    } else {
                        t!("theme.light")
                    },
                    bg = if self.dark {
                        t!("theme.bg_black")
                    } else {
                        t!("theme.bg_white")
                    }
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

    /// 顶栏动作按钮：全玻璃（嵌在 toolbar 里，见文件头注释的用户拍板说明）。
    pub(crate) fn action_buttons(&mut self, ui: &mut egui::Ui) -> Action {
        let mut act = Action::None;
        if egui_glass::GlassButton::new(t!("toolbar.open"))
            .show(ui)
            .clicked()
        {
            act = Action::Open;
        }
        if egui_glass::GlassButton::new(t!("toolbar.save"))
            .show(ui)
            .clicked()
        {
            act = Action::Save;
        }
        if egui_glass::GlassButton::new(t!("toolbar.save_as"))
            .show(ui)
            .clicked()
        {
            act = Action::SaveAs;
        }
        if egui_glass::GlassButton::new(t!("toolbar.reset"))
            .show(ui)
            .clicked()
        {
            act = Action::Reset;
        }
        ui.add_space(6.0);
        if egui_glass::GlassButton::new(if self.dark {
            t!("toolbar.to_light")
        } else {
            t!("toolbar.to_dark")
        })
        .show(ui)
        .clicked()
        {
            act = Action::ToggleTheme;
        }
        if egui_glass::GlassButton::new(if self.zh { "EN" } else { "中文" })
            .show(ui)
            .clicked()
        {
            act = Action::ToggleLang;
        }
        act
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

    /// 实时背景的页面层：只留一句提示做折射燃料。`live.run` 会让它跑两遍
    /// （可见 + 离屏），所以这里禁止任何写状态、禁止放玻璃件、
    /// 禁止放可交互控件——只许纯文本。
    /// 摘要行只放左侧导航卡，不在这里重复占右侧空间。
    pub(crate) fn page_backdrop(&self, page: &mut egui::Ui) {
        // 先铺底色：窗口清屏色不可控（实测透黑），两遍（可见 + 离屏）用同一底，
        // 映射才一致。深浅跟主题走。
        let bg = if self.dark {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        };
        page.painter().rect_filled(page.max_rect(), 0.0, bg);
        page.add_space(4.0);
        page.label(egui::RichText::new(t!("sum.hint")).weak().small());
        page.add_space(4.0);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 无 GPU 时的兜底：单面板堆叠。玻璃管线没注册，玻璃件不可用，
        // 这里只能用普通控件（该分支实际走不到，renderer 已强制 Wgpu）。
        if !self.glass {
            let mut act = Action::None;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.page, Page::File, t!("nav.file"));
                    ui.selectable_value(&mut self.page, Page::Accel, t!("nav.accel"));
                    ui.selectable_value(&mut self.page, Page::Player, t!("nav.player"));
                    ui.selectable_value(&mut self.page, Page::Wallpaper, t!("nav.wallpaper"));
                    ui.selectable_value(&mut self.page, Page::Boot, t!("nav.boot"));
                    ui.selectable_value(&mut self.page, Page::Summary, t!("nav.summary"));
                    if ui.button(t!("toolbar.open")).clicked() {
                        act = Action::Open;
                    }
                    if ui.button(t!("toolbar.save")).clicked() {
                        act = Action::Save;
                    }
                    if ui.button(if self.zh { "EN" } else { "中文" }).clicked() {
                        act = Action::ToggleLang;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                ui.separator();
                ui.label(&self.status);
            });
            self.do_action(ui.ctx(), act);
            return;
        }

        // （实时背景在中央面板内容头，见下。）

        // 顶栏：文件动作工具条胶囊（误伤恢复：真凶是 Panel 分隔线，现已全关，
        // 胶囊本身圆角无直边）。
        let mut act = Action::None;
        egui::Panel::top("toolbar")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    egui_glass::GlassToolbar::new(self.bar_style()).show(ui, |ui| {
                        // 顶栏不放标题（窗口标题栏 + 导航/文件页已有，不重复）。
                        act = self.action_buttons(ui);
                    });
                });
                ui.add_space(8.0);
            });

        // 底栏：状态胶囊（同上，误伤恢复）。
        egui::Panel::bottom("status")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.vertical_centered(|ui| {
                    egui_glass::GlassToolbar::new(self.bar_style()).show(ui, |ui| {
                        ui.label(&self.status);
                    });
                });
                ui.add_space(8.0);
            });

        // 左侧：玻璃导航卡。
        egui::Panel::left("nav")
            .default_size(216.0)
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                let card = if self.dark {
                    egui_glass::Glass::panel_dark()
                } else {
                    egui_glass::Glass::panel()
                };
                card.inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.heading(t!("nav.sections"));
                    self.nav_button(ui, Page::File, t!("nav.file").into_owned());
                    self.nav_button(ui, Page::Accel, t!("nav.accel").into_owned());
                    self.nav_button(ui, Page::Player, t!("nav.player").into_owned());
                    self.nav_button(ui, Page::Wallpaper, t!("nav.wallpaper").into_owned());
                    self.nav_button(ui, Page::Boot, t!("nav.boot").into_owned());
                    self.nav_button(ui, Page::Summary, t!("nav.summary").into_owned());
                    ui.add_space(8.0);
                    ui.small(t!("sum.accel", v = self.cfg.accel.level.file_str()));
                    ui.small(t!("sum.render", v = self.cfg.render.mode.file_str()));
                    ui.small(t!("sum.workers", v = self.cfg.render.workers.file_str()));
                    ui.small(t!("sum.dist", v = self.cfg.render.dist.file_str()));
                    ui.small(t!("sum.vsync", v = self.cfg.render.vsync.file_str()));
                    ui.small(t!(
                        "sum.wallpaper",
                        v = if self.cfg.wallpaper.enable {
                            "on"
                        } else {
                            "off"
                        }
                    ));
                    ui.small(t!("sum.play", v = self.play_text()));
                    ui.small(self.measured_line());
                    ui.small(t!("sum.version", v = self.cfg.version));
                });
                ui.add_space(8.0);
            });

        // 中央：大号玻璃薄片（误伤恢复：之前误判薄片直边是分割线，
        // 实为 Panel 分隔线，现已全关）。稠密表单放 panel frost 上。
        // 中央最后加（Panel 之后）。
        //
        // 实时背景只能接在这里：`live.run` 要求调用时光标全新（twin 用全新
        // Ui 重排一次，两遍光标必须一致）——根 ui 上画页面会把 Panel 全挤下去，
        // 独立 Area 实测也会打乱 Panel 布局（原因未明，宁可信截屏），
        // 只有中央面板内容头是安全的：兄弟 Panel 已先占位，内部只是多滚一段。
        // 页面是弱化摘要纸（折射燃料），表单跟在后面正常滚。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let sheet = if self.dark {
                    egui_glass::Glass::panel_dark()
                } else {
                    egui_glass::Glass::panel()
                };
                sheet.inner_margin(egui::Margin::same(16)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    if let Some(live) = self.live.as_ref() {
                        let clear = if self.dark {
                            egui::Color32::BLACK
                        } else {
                            egui::Color32::WHITE
                        };
                        live.run(ui, clear, |page| self.page_backdrop(page));
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                });
            });

        self.do_action(ui.ctx(), act);
    }
}
