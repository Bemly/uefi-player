//! 应用壳：`App` 状态 + `Page` 导航 + `eframe::App` 实现。
//! 页面内容见 `pages`，状态同步/持久化见 `state`，玻璃小件见 `widgets`，
//! 导航与动作见 `nav`（同 `impl App`，跨文件只为守 1000 行上限，行为零改）。

use eframe::egui;
use rust_i18n::t;

use crate::decode_bg;
use crate::gallery::GalleryState;
use crate::schema::*;
use crate::transcode::TranscodeState;

/// 导航与动作（`app_nav.rs`，同式挂载，行为零改）。
#[path = "app_nav.rs"]
pub(crate) mod nav;
/// 播放列表编辑器（`playlist.rs`，同 `impl App`，跨文件只为守 1000 行上限；
/// `#[path]` 挂载：`main.rs` 未动（不可编辑），故在此声明子模块，
/// 编译单元为 `crate::app::playlist`，行为与顶层 `mod playlist` 一致）。
#[path = "playlist.rs"]
pub(crate) mod playlist;
/// 状态同步与持久化（`app_state.rs`，`app.rs` 触达即拆，`#[path]` 挂载，行为零改）。
#[path = "app_state.rs"]
pub(crate) mod state;
/// 中央透明区玻璃小件（`app_widgets.rs`，同式挂载，行为零改）。
#[path = "app_widgets.rs"]
pub(crate) mod widgets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    File,
    Accel,
    Decode,
    Wallpaper,
    Boot,
    Transcode,
    Gallery,
    Summary,
}

/// 解码页顶部固定标签（只切展示，不落盘；格式位与 `DecodeTable::ORDER` 同序，
/// 首位 `Global` 是全局默认帧率 + 供片）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeTab {
    Global,
    Format(MediaFormat),
}

impl DecodeTab {
    pub(crate) const ALL: [Self; 9] = [
        Self::Global,
        Self::Format(MediaFormat::Qois),
        Self::Format(MediaFormat::Raw),
        Self::Format(MediaFormat::Hevc),
        Self::Format(MediaFormat::Vp9),
        Self::Format(MediaFormat::H263),
        Self::Format(MediaFormat::Mpeg2v),
        Self::Format(MediaFormat::Theora),
        Self::Format(MediaFormat::Avc),
    ];
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
    /// （播放页已并入解码页顶：全局默认帧率 + 供片摆在 8 tabs 上方。）
    pub(crate) play_f: f32,
    /// 供片模式滑杆镜像：0-4 对应 `FeedMode::ALL` 下标（`sync_widgets` 随读盘同步，
    /// 否则开档文件载入后滑杆仍停在旧位，下一帧会把已载入的档位盖掉）。
    /// 家在解码页顶（`[play] feed` 全局默认，逐格式 `fetch` 覆盖它）。
    pub(crate) feed_f: f32,
    /// 循环模式滑杆镜像：0-2 对应 `LoopMode::ALL` 下标（`sync_widgets` 随读盘同步）。
    /// P5 起 `[playlist]` 即壁纸画廊（播放改走 `[play] media_dir` 整目录）：
    /// 本镜像仍读写 `cfg.playlist.loop_mode` 同键，只换页签与注释，逻辑零改。
    pub(crate) playlist_loop_f: f32,
    /// 解码页当前 tab（全局 + 8 格式单选，默认全局；只切展示，不落盘）。
    pub(crate) decode_tab: DecodeTab,
    /// 列表单选（`None` = 未选；GlassButton 行点选，删除/上下移消费它）。
    /// （P5：`[[wallpaper.items]]` 已删，`wallpaper_sel` 同删；画廊单选只剩 `playlist_sel`。）
    pub(crate) playlist_sel: Option<usize>,
    /// 解码表滑杆镜像（与 `DecodeTable::ORDER` 同序，同下标）：
    /// 线程 0-1 对应 `DecodeThreads::ALL` 下标，缓冲 0-32（0 = 跟随 `tune.window`），
    /// 供片 0 = 继承全局 `[play] feed`（`None`，落盘不写键），1-5 对应 `FeedMode::ALL` 下标 +1。
    pub(crate) dec_threads_f: [f32; 8],
    pub(crate) dec_buffers_f: [f32; 8],
    pub(crate) dec_fetch_f: [f32; 8],
    /// 加速档位滑杆镜像：0-30 对应 `AccelLevel::ALL` 下标（`sync_widgets` 随读盘同步，
    /// 否则开档文件载入后滑杆仍停在旧位，下一帧会把已载入的档位盖掉）。
    pub(crate) accel_f: f32,
    /// 壁纸滑杆镜像：fps 1-30，preload 0-32（0 = 不封顶）。
    /// 库内滑杆只吃 `f32`，整数档用 round 镜像（见 Accel 页 `*_f` 字段）。
    /// 旧 `frames`（过渡封顶）已删：列表长度即过渡长度。
    pub(crate) wp_fps_f: f32,
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
    /// 核心上限滑杆镜像：0..=64（0 = 自动不限，库内滑杆只吃 `f32`，整数档用 round 镜像）。
    pub(crate) sched_cores_f: f32,
    /// 黑边 RGB 滑杆镜像：0..=255（库内滑杆只吃 `f32`，整数档用 round 镜像；
    /// 预设行点选后同步三值，滑杆每帧写回 `cfg.display.bar`）。
    pub(crate) bar_r_f: f32,
    pub(crate) bar_g_f: f32,
    pub(crate) bar_b_f: f32,
    pub(crate) glass: bool,
    /// 深色模式：egui visuals + 纯黑 backdrop + 深色玻璃预设三处联动。
    pub(crate) dark: bool,
    /// 中文模式：`rust_i18n::set_locale` 全局切换，文案全走 `t!`。
    pub(crate) zh: bool,
    /// 实时背景：页面跑两遍（可见 + 离屏折射源），开关浮在上层。无 GPU 时为 None。
    pub(crate) live: Option<egui_glass::LiveBackdrop>,
    /// twin 折射源里的背景图（与可见遍同一张片源第一帧；native id 在
    /// `LiveBackdrop::new` 之后登记，自动镜像到双胞胎渲染器，离屏画安全）。
    /// 可见遍用 glass 自家的静态底同一张图；twin 另挂一份是因 twin 读不到
    /// 主 ctx 的 backdrop 状态（`show_backdrop` 在 twin 里是空转，实测过）。
    /// 无 GPU 时为 None（回退纯色，不断任何流程）。
    /// `_tex`/`_view` 只为保活（native id 悬空即 UB 级黑屏，见保留注释）。
    /// 动态视频有首帧时不画它（视频 native 同矩形盖位，避免双图叠画）。
    pub(crate) twin_photo: Option<TwinPhoto>,
    /// 动态背景（GPU 转换版：NV12 上传 + WGSL 转 RGBA，CPU 零像素运算；
    /// 只在 `live.run` 内画当前 native 帧，无帧回静态底，不断流程）。
    pub(crate) video_bg: crate::video_bg::VideoBg,
    /// 图库状态（目录跟 `player.toml` 路径走）。
    pub(crate) gallery: GalleryState,
    /// 转码状态（worker 线程跑，UI 只轮询）。
    pub(crate) transcode: TranscodeState,
    /// rfd 文件对话框会话（打开/保存/播放列表加项各持一个：winit macOS 禁止
    /// 事件处理内再进事件循环，对话框必须后台线程跑阻塞版，UI 侧每帧收结果，
    /// 见 `file_pick` 头注释血泪）。
    pub(crate) open_pick: crate::file_pick::FilePick,
    pub(crate) save_pick: crate::file_pick::FilePick,
    pub(crate) list_pick: crate::file_pick::FilePick,
    /// 右上 toast：`status` 一变就弹，`TOAST_SECS` 后自动收。
    toast: Option<Toast>,
    /// toast 已播记忆：播过的 status 不再续弹（否则过期即重建变循环播放）。
    toast_seen: String,
    /// 右下动作条像素图标（CC0 位图，见 `assets/icons/README.md`；
    /// `App::new` 一次载入，`TextureHandle` 保活）。
    icons: Icons,
}

/// 动作条图标（24 栅格单色 PNG，`NEAREST` 直显；主题占两席）。
pub(crate) struct Icons {
    pub(crate) open: egui::TextureHandle,
    pub(crate) save: egui::TextureHandle,
    pub(crate) save_as: egui::TextureHandle,
    pub(crate) reset: egui::TextureHandle,
    pub(crate) theme_dark: egui::TextureHandle,
    pub(crate) theme_light: egui::TextureHandle,
    pub(crate) lang: egui::TextureHandle,
}

impl Icons {
    fn load(ctx: &egui::Context) -> Self {
        fn one(ctx: &egui::Context, name: &str, bytes: &[u8]) -> egui::TextureHandle {
            let img = image::load_from_memory(bytes)
                .expect("bundled icon decodes")
                .to_rgba8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            let px = img.into_raw();
            ctx.load_texture(
                name,
                egui::ColorImage::from_rgba_unmultiplied([w, h], &px),
                egui::TextureOptions::NEAREST,
            )
        }
        Self {
            open: one(ctx, "icon-open", include_bytes!("../assets/icons/open.png")),
            save: one(ctx, "icon-save", include_bytes!("../assets/icons/save.png")),
            save_as: one(
                ctx,
                "icon-save-as",
                include_bytes!("../assets/icons/save_as.png"),
            ),
            reset: one(
                ctx,
                "icon-reset",
                include_bytes!("../assets/icons/reset.png"),
            ),
            theme_dark: one(
                ctx,
                "icon-theme-dark",
                include_bytes!("../assets/icons/theme_dark.png"),
            ),
            theme_light: one(
                ctx,
                "icon-theme-light",
                include_bytes!("../assets/icons/theme_light.png"),
            ),
            lang: one(ctx, "icon-lang", include_bytes!("../assets/icons/lang.png")),
        }
    }
}

/// twin 用背景图（纹理本体 + 视图保活 + 几何，随 id 一起存）。
pub(crate) struct TwinPhoto {
    pub(crate) id: egui::TextureId,
    pub(crate) w: f32,
    pub(crate) h: f32,
    #[allow(dead_code)]
    tex: egui_wgpu::wgpu::Texture,
    #[allow(dead_code)]
    view: egui_wgpu::wgpu::TextureView,
}

/// 右上 toast 一次弹出的内容 + 起始时刻（`status` 文本一变就续一次）。
struct Toast {
    text: String,
    since: std::time::Instant,
}

/// toast 存活秒数：进度条走完自动收（用户拍板：常驻底栏改右上倒计时弹窗）。
const TOAST_SECS: f32 = 3.0;

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

        // 默认英文 + 深色（用户拍板：打开即深色），与初始 backdrop 一致。
        // 窗口标题跟语言走（中英切换时见 ToggleLang），首帧即按当前 locale 下发。
        rust_i18n::set_locale("en");
        cc.egui_ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").into_owned()));
        // 主题偏好显式锁深色（不跟系统）：egui 按 `theme()` 选槽写 visuals，
        // 偏好默认 `System` 时 `App::new` 执行瞬间系统主题输入还没进内存，
        // `set_visuals` 会写进 Dark 槽而首帧起 UI 读的是系统浅色对应的 Light 槽
        // （裸 `ui.label` 全跪，只有走自家 `self.dark` 的描边字是对的；
        // 切一次浅色再切回来才好就是这个原因）。锁死后两槽各归其位。
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let mut glass = false;
        let mut live = None;
        let mut twin_photo = None;
        if let Some(r) = cc.wgpu_render_state.as_ref() {
            // MSAA 数与 NativeOptions 一致（默认不开 MSAA，传 1）。
            egui_glass::init(r, 1);
            // 顺序按官方要求：init → live（live 常开，玻璃恒采 twin，
            // 静态底无人采样，不再 `set_backdrop`，省一次上传）。
            live = Some(egui_glass::LiveBackdrop::new(r, Some(live_fonts)));
            // twin 折射源图：与可见遍同一包，Rgba8 直传（行距 w*4 天然 256 对齐，
            // `write_texture` 无需补齐）；native id 登记（live 之后，自动镜像）。
            if let Some((w, h, px)) = decode_bg() {
                let tex = r
                    .device
                    .create_texture(&egui_wgpu::wgpu::TextureDescriptor {
                        label: Some("bg photo twin"),
                        size: egui_wgpu::wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: egui_wgpu::wgpu::TextureDimension::D2,
                        format: egui_wgpu::wgpu::TextureFormat::Rgba8Unorm,
                        usage: egui_wgpu::wgpu::TextureUsages::TEXTURE_BINDING
                            | egui_wgpu::wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                r.queue.write_texture(
                    egui_wgpu::wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: egui_wgpu::wgpu::Origin3d::ZERO,
                        aspect: egui_wgpu::wgpu::TextureAspect::All,
                    },
                    &px,
                    egui_wgpu::wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(h),
                    },
                    egui_wgpu::wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                let view = tex.create_view(&Default::default());
                let id = egui_glass::register_native_texture(r, &view);
                twin_photo = Some(TwinPhoto {
                    id,
                    w: w as f32,
                    h: h as f32,
                    tex,
                    view,
                });
            }
            glass = true;
        }
        let icons = Icons::load(&cc.egui_ctx);
        // 视频 GPU 转换器要 RenderState（device/queue 克隆；转换提交在 tick 内同步做，
        // paint 回调里零逻辑，避 nounwind abort）；无 GPU（None）即全程静态底。
        let video_bg = crate::video_bg::VideoBg::new(cc.wgpu_render_state.clone());
        let mut app = Self {
            path: "player.toml".to_owned(),
            cfg: FileConfig::default(),
            status: t!("status.ready").into_owned(),
            page: Page::File,
            play_f: 0.0,
            feed_f: FeedMode::Clip.index() as f32,
            playlist_loop_f: LoopMode::LoopList.index() as f32,
            decode_tab: DecodeTab::Global,
            playlist_sel: None,
            dec_threads_f: [0.0; 8],
            dec_buffers_f: [0.0; 8],
            dec_fetch_f: [0.0; 8],
            accel_f: AccelLevel::Auto.index() as f32,
            wp_fps_f: 15.0,
            wp_preload_f: default_wallpaper_preload() as f32,
            tune_frames_f: default_frames() as f32,
            tune_tile_f: default_tile_rows() as f32,
            tune_vsync_fps_f: default_vsync_fps() as f32,
            tune_buffers_f: default_buffers() as f32,
            tune_at_load: RenderTune::default(),
            sched_cores_f: 0.0,
            bar_r_f: 0.0,
            bar_g_f: 0.0,
            bar_b_f: 0.0,
            glass,
            dark: true,
            zh: false,
            live,
            twin_photo,
            video_bg,
            gallery: GalleryState::new(),
            transcode: TranscodeState::new(),
            open_pick: crate::file_pick::FilePick::new(),
            save_pick: crate::file_pick::FilePick::new(),
            list_pick: crate::file_pick::FilePick::new(),
            toast: None,
            toast_seen: String::new(),
            icons,
        };
        // 启动时若工作目录下已有 player.toml，直接载入。
        if std::path::Path::new(&app.path).exists() {
            let p = app.path.clone();
            app.load_path(p);
        }
        // 启动即刷图库：默认背景跟随图库首个 avc/hevc，不等人点刷新。
        let p = app.path.clone();
        app.gallery.refresh(&p);
        app
    }

    /// 滑杆/摘要共用的档位文本（P5 起 `[playlist]` 即壁纸画廊：帧率 × 画廊条目数 × 循环；
    /// 公式零改，只换语义注释；播放供片改走 `[play] media_dir` 整目录）。
    pub(crate) fn play_text(&self) -> String {
        let base = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => t!("play.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n),
        };
        format!(
            "{} x {} ({})",
            base,
            self.cfg.playlist.items.len(),
            self.cfg.playlist.loop_mode.file_str()
        )
    }

    /// 壁纸摘要行（P5：`[wallpaper] loop/items` 已删；开关 × 帧率 × 预载 × 冻结。
    /// 画廊条目/循环看 `[playlist]`，见 `play_text` 与壁纸页画廊节）。
    pub(crate) fn wallpaper_text(&self) -> String {
        if !self.cfg.wallpaper.enable {
            return String::from("off");
        }
        format!(
            "on {}/pre{}/fz{}",
            self.wp_fps_text(),
            self.cfg.wallpaper.preload,
            self.cfg.wallpaper.popup_freeze as u8,
        )
    }

    /// 壁纸帧率滑杆文本（三态与播放同构；auto 跟 vsync，详见固件 `effective_wallpaper_fps`）。
    pub(crate) fn wp_fps_text(&self) -> String {
        match parse_fps(&self.cfg.wallpaper.fps) {
            FpsPolicy::Auto => t!("wallpaper.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n.clamp(1, 240)),
        }
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

    /// 渲染矩阵记忆行：全未测提示切 auto，否则报记忆值 + `cores n/m`
    ///（n = `[schedule] measured_workers_n` 首轮 sweep 胜者，m = 校准时机型总数）。
    pub(crate) fn render_measured_line(&self) -> String {
        let m = &self.cfg.render;
        let n = self.cfg.schedule.measured_workers_n;
        if m.measured_render == RenderMode::Auto
            && m.measured_workers == WorkersMode::Auto
            && m.measured_dist == DistMode::Auto
            && m.measured_vsync == VsyncMode::Auto
            && m.measured_cores == 0
            && n == 0
        {
            t!("render.unmeasured").into_owned()
        } else {
            t!(
                "render.measured",
                v = format!(
                    "{}/{}/{}/{}, cores {}/{}",
                    m.measured_render.file_str(),
                    m.measured_workers.file_str(),
                    m.measured_dist.file_str(),
                    m.measured_vsync.file_str(),
                    n,
                    m.measured_cores
                )
            )
            .into_owned()
        }
    }

    /// 调度 sweep 记忆行（只读展示）：0 = 未测，下次启动 sweep 并持久化胜者；
    /// 有值报 `cores n/m`（n = 胜者，m = 校准时机型总数）。
    pub(crate) fn schedule_measured_line(&self) -> String {
        let n = self.cfg.schedule.measured_workers_n;
        let m = self.cfg.render.measured_cores;
        if n == 0 {
            t!("sched.unmeasured").into_owned()
        } else {
            t!("sched.measured", n = n, m = m).into_owned()
        }
    }

    /// 固件串口行同式摘要（仅展示）：`sched: crew=<n>/<m> roles=<r> workers=<w>`
    ///（n = sweep 胜者，m = 校准时机型总数；未测时 n/m = 0/measured_cores）。
    pub(crate) fn sched_crew_line(&self) -> String {
        format!(
            "sched: crew={}/{} roles={} workers={}",
            self.cfg.schedule.measured_workers_n,
            self.cfg.render.measured_cores,
            self.cfg.schedule.roles.file_str(),
            self.cfg.schedule.workers.file_str(),
        )
    }
}

/// 背景图右移量（pt，见 `cover_shifted`；216 宽导航，取 180 可调）。
pub(crate) const BG_SHIFT_X: f32 = 180.0;

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
                    ui.selectable_value(&mut self.page, Page::Decode, t!("nav.decode"));
                    ui.selectable_value(&mut self.page, Page::Wallpaper, t!("nav.wallpaper"));
                    ui.selectable_value(&mut self.page, Page::Boot, t!("nav.boot"));
                    ui.selectable_value(&mut self.page, Page::Transcode, t!("nav.transcode"));
                    ui.selectable_value(&mut self.page, Page::Gallery, t!("nav.gallery"));
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
                // 解码 tab 条钉在滚动区之外（无 GPU 兜底分支同式）。
                if self.page == Page::Decode {
                    self.decode_tab_bar(ui);
                }
                egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                ui.separator();
                ui.label(&self.status);
            });
            self.do_action(ui.ctx(), act);
            return;
        }

        // 实时背景必须在全部玻璃之前跑：`live.run` 同步设置本帧 `LiveState`，
        // 之后所有 `Glass*` 才能拿到全屏 backdrop_rect。若把它放在中央面板内容头，
        // 顶栏/导航的玻璃在同帧先画，只能回退到自身 rect 做 backdrop，
        // 就会把整张底图压进每颗按钮（2026-10-22 截屏故障）。
        // 代价是 `add_contents` 只能做纯绘制（painter image，不占布局），
        // 否则根光标会被挤、Panel 全被顶下去。文字摘要不在折射源里，
        // 只做中央薄片内的普通内容。
        // 先画的在下：背景图在最底，之后全部面板浮在上。右移让脸出导航
        // （见 `cover_shifted`）。无纸罩（用户明确不要白色遮罩）。
        // 动态优先：有视频首帧即画视频 GPU native 帧（与静态同矩形含右移，
        // 只在 live 内画，玻璃折射源）；无帧/回退即静态 twin 图。
        let screen = ui.ctx().viewport_rect();
        // 推进只能在闭包外做（`live.run` 闭包禁写状态/禁时钟，见上游 `live.rs`；
        // 转换提交也在 tick 内同步做完，paint 回调零逻辑，避 nounwind abort）。
        if self.glass {
            // 三级默认：选中接管 > 图库首个 avc/hevc > 内嵌 `dsxhgm.mp4`。
            // 会话级，无落盘（转码页状态本就不进 player.toml）。
            let want = crate::video_bg::VideoBg::default_source(
                &self.gallery.entries,
                self.gallery.selected,
                &crate::gallery::gallery_dir(&self.path),
            );
            self.video_bg.set_source(Some(want));
            self.video_bg.tick(ui.ctx());
        }
        if let Some(live) = self.live.as_ref() {
            let clear = if self.dark {
                egui::Color32::BLACK
            } else {
                egui::Color32::WHITE
            };
            live.run(ui, clear, |page| {
                // 注意：只画图，不加 label/分配，保持根光标全新。
                if let Some((id, w, h)) = self.video_bg.front() {
                    // 动态帧（GPU native；twin 自动镜像，折射与所见一致）。
                    let r = Self::cover_shifted(screen, w, h, BG_SHIFT_X);
                    page.painter().with_clip_rect(screen).image(
                        id,
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    return;
                }
                // twin 图进折射源（native id 自动映射；离屏画不影响可见合成；
                // 与可见遍同矩形（含右移），折射与所见一致）。
                if let Some(t) = self.twin_photo.as_ref() {
                    let r = Self::cover_shifted(screen, t.w, t.h, BG_SHIFT_X);
                    page.painter().with_clip_rect(screen).image(
                        t.id,
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            });
        }

        // 动作条：右下角悬浮胶囊（用户拍板：顶部常驻改悬浮，像图二右下角）。
        // `Area` 前景层锚右下，不占 Panel 布局；里面仍是玻璃工具条 +
        // 玻璃按钮（件库约束不变）。底栏常驻状态条已摘除，改右上 toast。
        // 顶栏不放标题（窗口标题栏 + 导航/文件页已有，不重复）。
        let mut act = Action::None;

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
                    // 纯白/纯黑（用户拍板：不用 visuals 默认灰，与中央描边字对齐）。
                    let nav_fg = crate::theme::fg(self.dark);
                    ui.heading(egui::RichText::new(t!("nav.sections")).color(nav_fg));
                    self.nav_button(ui, Page::File, t!("nav.file").into_owned());
                    self.nav_button(ui, Page::Accel, t!("nav.accel").into_owned());
                    self.nav_button(ui, Page::Decode, t!("nav.decode").into_owned());
                    self.nav_button(ui, Page::Wallpaper, t!("nav.wallpaper").into_owned());
                    self.nav_button(ui, Page::Boot, t!("nav.boot").into_owned());
                    self.nav_button(ui, Page::Transcode, t!("nav.transcode").into_owned());
                    self.nav_button(ui, Page::Gallery, t!("nav.gallery").into_owned());
                    self.nav_button(ui, Page::Summary, t!("nav.summary").into_owned());
                    ui.add_space(8.0);
                    // 摘要小字同样纯白/纯黑（用户拍板：不用 visuals 默认灰）。
                    for s in [
                        t!("sum.accel", v = self.cfg.accel.level.file_str()).into_owned(),
                        t!("sum.render", v = self.cfg.render.mode.file_str()).into_owned(),
                        t!("sum.workers", v = self.cfg.schedule.workers.file_str()).into_owned(),
                        t!(
                            "sum.cores",
                            v = if self.cfg.schedule.cores == 0 {
                                String::from("auto")
                            } else {
                                self.cfg.schedule.cores.min(64).to_string()
                            }
                        )
                        .into_owned(),
                        t!("sum.roles", v = self.cfg.schedule.roles.file_str()).into_owned(),
                        t!("sum.dist", v = self.cfg.schedule.dist.file_str()).into_owned(),
                        t!("sum.vsync", v = self.cfg.render.vsync.file_str()).into_owned(),
                        t!("sum.wallpaper", v = self.wallpaper_text()).into_owned(),
                        t!("sum.play", v = self.play_text()).into_owned(),
                        self.measured_line(),
                        t!("sum.version", v = self.cfg.version).into_owned(),
                    ] {
                        ui.small(egui::RichText::new(s).color(nav_fg));
                    }
                });
                ui.add_space(8.0);
            });

        // 中央：透明内容区（用户拍板：不要遮罩，直接透底图；其它框不动）。
        // 稠密表单直接画在底图上，静态文字走描边（`content_*`），
        // 里面自带的 GlassButton/GlassSlider 控件保留玻璃（件库约束）。
        // 中央最后加（Panel 之后）。折射源已在根部 `live.run` 跑完，
        // 这里不再跑第二遍 `live.run`（两遍会互盖，谁后跑谁赢，
        // 且早画的玻璃已错过第一遍）。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        // 中央字号整体放大一档（用户：中间内容再大一点）。
                        // 默认 Body 13 / Small 9 / Heading 18 / Mono 13，
                        // 这里只改中央这个 Ui 的 style，不影响顶栏/导航/状态栏。
                        {
                            let mut style = (**ui.style()).clone();
                            for font_id in style.text_styles.values_mut() {
                                font_id.size += 3.0;
                            }
                            ui.set_style(style);
                        }
                        ui.set_min_size(ui.available_size());
                        self.page_backdrop(ui);
                        // 解码 tab 条钉死在滚动区之外、卡片最上面（不随内容滚动）。
                        // 只能放这里：根 ui 上画会把 Panel 挤下去（见上注释），
                        // 页内画会被 ScrollArea 卷走——内容头是唯一安全位。
                        if self.page == Page::Decode {
                            self.decode_tab_bar(ui);
                        }
                        egui::ScrollArea::vertical().show(ui, |ui| self.page_content(ui));
                    });
            });

        // 悬浮动作条最后画（前景层，盖在中央内容右下角，不挤任何布局）。
        egui::Area::new("float_actions".into())
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .show(ui.ctx(), |ui| {
                act = self.action_buttons(ui);
            });

        self.show_toast(ui.ctx());

        self.do_action(ui.ctx(), act);
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    #[test]
    fn shifted_face_clears_nav() {
        // 1024x712 窗 + 1280x720 图 + 右移 180：盖住全屏是硬约束
        // （`sized.w >= sw + 2*shift`，否则左侧露 `clear` 白边）；
        // 脸是尽力而为：片源脸（tex x 180..350）落窗 x ~195..378，
        // 左脸颊擦边 216 导航（糊区无完整五官，不成重影）；右缘不出窗。
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1024.0, 712.0));
        let r = App::cover_shifted(screen, 1280.0, 720.0, BG_SHIFT_X);
        // 硬约束：平移后仍盖住全屏，无白边。
        assert!(r.min.x <= 0.0 && r.max.x >= 1024.0, "covers horizontally");
        assert!(r.min.y <= 0.0 && r.max.y >= 712.0, "covers vertically");
        let face = |tx: f32| r.min.x + tx / 1280.0 * r.width();
        assert!(
            face(180.0) > 170.0,
            "face mostly clears nav (cheek overlap ok)"
        );
        assert!(face(350.0) < 1024.0, "face right edge on screen");
        // 同矩形在 twin/可见两遍共用（折射与所见一致），尺寸恒定。
        // scale = max(1024/1280, 712/720, (1024+360)/1280) = 1.08125。
        assert!((r.width() - 1384.0).abs() < 0.01 && (r.height() - 778.5).abs() < 0.01);
    }
}
