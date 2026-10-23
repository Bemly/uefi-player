//! 应用壳：`App` 状态 + `Page` 导航 + 除 `page_content` 外的全部方法 + `eframe::App` 实现。
//! 页面内容见 `pages`（同 `impl App`，跨文件只为守 1000 行上限）。

use eframe::egui;
use rust_i18n::t;

use crate::decode_bg;
use crate::gallery::GalleryState;
use crate::rules::*;
use crate::schema::*;
use crate::text::*;
use crate::transcode::TranscodeState;

/// 播放列表编辑器（`playlist.rs`，同 `impl App`，跨文件只为守 1000 行上限；
/// `#[path]` 挂载：`main.rs` 未动（不可编辑），故在此声明子模块，
/// 编译单元为 `crate::app::playlist`，行为与顶层 `mod playlist` 一致）。
#[path = "playlist.rs"]
pub(crate) mod playlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    File,
    Accel,
    Playlist,
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
    /// 播放列表与壁纸画廊各一（加速/供片滑杆同式，无新件）。
    pub(crate) playlist_loop_f: f32,
    pub(crate) wp_loop_f: f32,
    /// 解码页当前 tab（全局 + 8 格式单选，默认全局；只切展示，不落盘）。
    pub(crate) decode_tab: DecodeTab,
    /// 列表单选（`None` = 未选；GlassButton 行点选，删除/上下移消费它）。
    pub(crate) playlist_sel: Option<usize>,
    pub(crate) wallpaper_sel: Option<usize>,
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
            wp_loop_f: LoopMode::LoopList.index() as f32,
            decode_tab: DecodeTab::Global,
            playlist_sel: None,
            wallpaper_sel: None,
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
        app
    }

    pub(crate) fn sync_widgets(&mut self) {
        self.accel_f = self.cfg.accel.level.index() as f32;
        self.feed_f = self.cfg.play.feed.index() as f32;
        self.playlist_loop_f = self.cfg.playlist.loop_mode.index() as f32;
        self.wp_loop_f = self.cfg.wallpaper.loop_mode.index() as f32;
        // 切表不跟读盘走：tab 只切展示，读盘保持当前 tab（见解码页）。
        for (i, fmt) in DecodeTable::ORDER.iter().enumerate() {
            if let Some(d) = self.cfg.decode.get(*fmt) {
                self.dec_threads_f[i] = d.threads.index() as f32;
                self.dec_buffers_f[i] = d.buffers.min(32) as f32;
                self.dec_fetch_f[i] = match d.fetch {
                    None => 0.0,
                    Some(f) => f.index() as f32 + 1.0,
                };
            }
        }
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
        self.wp_preload_f = self.cfg.wallpaper.preload.min(32) as f32;
        self.tune_frames_f = self.cfg.render.tune.frames.clamp(1, 120) as f32;
        self.tune_tile_f = self.cfg.render.tune.tile_rows.clamp(8, 512) as f32;
        self.tune_vsync_fps_f = self.cfg.render.tune.vsync_fps.clamp(15, 240) as f32;
        self.tune_buffers_f = self.cfg.render.tune.buffers.min(16) as f32;
        self.sched_cores_f = self.cfg.schedule.cores.min(64) as f32;
        self.bar_r_f = self.cfg.display.bar.r as f32;
        self.bar_g_f = self.cfg.display.bar.g as f32;
        self.bar_b_f = self.cfg.display.bar.b as f32;
        // 越界单选回到列表内（删行后旧下标悬空，下一帧点行即覆盖）。
        if let Some(s) = self.playlist_sel {
            if s >= self.cfg.playlist.items.len() {
                self.playlist_sel = None;
            }
        }
        if let Some(s) = self.wallpaper_sel {
            if s >= self.cfg.wallpaper.items.len() {
                self.wallpaper_sel = None;
            }
        }
    }

    /// 滑杆/摘要共用的档位文本（播放列表制：帧率 × 条目数 × 循环）。
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

    /// 壁纸摘要行（画廊列表制：开关 × 帧率 × 条目数 × 循环 × 预载 × 冻结）。
    pub(crate) fn wallpaper_text(&self) -> String {
        if !self.cfg.wallpaper.enable {
            return String::from("off");
        }
        format!(
            "on {}/{}/x{}/pre{}/fz{}",
            self.wp_fps_text(),
            self.cfg.wallpaper.loop_mode.file_str(),
            self.cfg.wallpaper.items.len(),
            self.cfg.wallpaper.preload,
            self.cfg.wallpaper.popup_freeze as u8,
        )
    }

    /// 壁纸帧率滑杆文本（三态与播放同构；auto 跟 vsync，详见固件 `effective_wallpaper_fps`）。
    pub(crate) fn wp_fps_text(&self) -> String {
        match parse_fps(&self.cfg.wallpaper.fps) {
            FpsPolicy::Auto => t!("wallpaper.text_auto").into_owned(),
            FpsPolicy::Max => t!("play.text_max").into_owned(),
            FpsPolicy::Capped(n) => format!("{} fps", n.min(240).max(1)),
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

    pub(crate) fn load_path(&mut self, path: String) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(cfg) => {
                    // 未知键 serde 默认忽略：老文件残留（`[play] pipeline` /
                    // `[render] workers/dist` / 别名值）不进解析，整文件失败才报错。
                    let cfg = coerce_loaded(cfg);
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
        clamp_decode(&mut self.cfg.decode);
        clamp_schedule(&mut self.cfg.schedule);
        // tune 变了 = bench 条件变了：旧记忆作废，下次启动重测。
        // 不做这个，改 tune 会被“有记忆快启”吞掉，永远不生效。
        if self.cfg.render.tune != self.tune_at_load {
            self.cfg.render.measured_render = RenderMode::Auto;
            self.cfg.render.measured_workers = WorkersMode::Auto;
            self.cfg.render.measured_dist = DistMode::Auto;
            self.cfg.render.measured_vsync = VsyncMode::Auto;
            self.cfg.render.measured_vsync_fps = 0;
            self.cfg.render.measured_cores = 0;
            self.cfg.schedule.measured_workers_n = 0;
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

    /// 右上 toast：`status` 文本一变就弹一次液态玻璃胶囊，底下细条是
    /// 消失进度条（`TOAST_SECS` 走完自动收）。库内无 toast/进度件，
    /// 玻璃胶囊 + 原语细条（注释说明）；一直有 toast 就一直要重绘。
    #[allow(clippy::too_many_lines)]
    pub(crate) fn show_toast(&mut self, ctx: &egui::Context) {
        if self.status != self.toast_seen {
            self.toast_seen = self.status.clone();
            self.toast = Some(Toast {
                text: self.status.clone(),
                since: std::time::Instant::now(),
            });
        }
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|t| t.since.elapsed().as_secs_f32() >= TOAST_SECS);
        if expired {
            self.toast = None;
            return;
        }
        let Some(t) = self.toast.as_ref() else {
            return;
        };
        ctx.request_repaint();
        let frac = 1.0 - t.since.elapsed().as_secs_f32() / TOAST_SECS;
        let accent = Self::accent(self.dark);
        let track = if self.dark {
            egui::Color32::from_white_alpha(28)
        } else {
            egui::Color32::from_black_alpha(28)
        };
        egui::Area::new("toast".into())
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 16.0))
            .show(ctx, |ui| {
                egui_glass::Glass::new(self.bar_style())
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        // 纯白/纯黑字（用户拍板：不要主题灰）：暗色白字，浅色黑字。
                        let fg = if self.dark {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::BLACK
                        };
                        ui.label(egui::RichText::new(&t.text).color(fg));
                        let w = ui.min_rect().width().max(120.0);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(w, 4.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, track);
                        let fill = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(w * frac.clamp(0.0, 1.0), 4.0),
                        );
                        ui.painter().rect_filled(fill, 2.0, accent);
                    });
            });
    }

    /// 工具栏/状态条的玻璃材质：深色配深色预设，浅色配默认。
    pub(crate) fn bar_style(&self) -> egui_glass::GlassStyle {
        if self.dark {
            egui_glass::GlassStyle::dark()
        } else {
            egui_glass::GlassStyle::regular()
        }
    }

    /// 中央透明区描边字（轮子结论：搜过 `egui` 文本描边，`RichText`/
    /// `TextFormat`（0.35）无原生 `stroke`，只有下划线/删除线；
    /// 故手写 1 物理像素八方向罩底 + 主字，库内无此件，注释说明。
    /// 只给中央静态文字用（玻璃上的按钮/滑杆不动）。
    /// 关联函数（不借 `self`）：拖拽行等闭包里只有预拷贝的 `dark`，
    /// 直接用它，不用再借外层 `self`。
    pub(crate) fn outlined_ui(
        ui: &mut egui::Ui,
        dark: bool,
        text: impl Into<egui::WidgetText>,
        fallback: egui::TextStyle,
    ) -> egui::Response {
        let (fg, stroke) = if dark {
            (egui::Color32::WHITE, egui::Color32::BLACK)
        } else {
            (egui::Color32::BLACK, egui::Color32::WHITE)
        };
        // 剥掉显式颜色再排版：整段走占位色，绘制色才能全覆盖——
        // 否则 `weak` 灰的描边也会被烤成灰色（2026-10-21 故障）。
        // 字号/换行保留（`fallback` 只管无样式段）。
        let wt: egui::WidgetText = text.into();
        let mut job = std::sync::Arc::unwrap_or_clone(wt.into_layout_job(
            ui.style(),
            fallback.into(),
            ui.text_valign(),
        ));
        for s in &mut job.sections {
            s.format.color = egui::Color32::PLACEHOLDER;
        }
        job.wrap = egui::text::TextWrapping::from_wrap_mode_and_width(
            ui.wrap_mode(),
            ui.available_width(),
        );
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        let size = galley.size();
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            // 2 物理像素（用户拍板）：pt 按像素密度折，保证 10px 像素字不断裂。
            let r = 2.0 / ui.ctx().pixels_per_point();
            for dy in [-r, 0.0, r] {
                for dx in [-r, 0.0, r] {
                    if dx == 0.0 && dy == 0.0 {
                        continue;
                    }
                    ui.painter()
                        .galley(rect.min + egui::vec2(dx, dy), galley.clone(), stroke);
                }
            }
            // 主字加粗：像素字无 bold 字重，横向双打 1 物理像素增粗
            // （浅色黑字/暗色白字同式，保持描边中心对称）。
            for ox in [-r * 0.5, r * 0.5] {
                ui.painter()
                    .galley(rect.min + egui::vec2(ox, 0.0), galley.clone(), fg);
            }
        }
        response
    }

    fn outlined(
        &self,
        ui: &mut egui::Ui,
        text: impl Into<egui::WidgetText>,
        fallback: egui::TextStyle,
    ) -> egui::Response {
        Self::outlined_ui(ui, self.dark, text, fallback)
    }

    /// 中央静态文字四件套（分别对应 `heading/label/small/monospace` 的字号，
    /// 颜色统一按主题白/黑 + 反色描边；`RichText` 的弱化/加粗等尺寸信息保留）。
    pub(crate) fn content_heading(
        &self,
        ui: &mut egui::Ui,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        self.outlined(ui, text, egui::TextStyle::Heading)
    }

    pub(crate) fn content_label(
        &self,
        ui: &mut egui::Ui,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        self.outlined(ui, text, egui::TextStyle::Body)
    }

    pub(crate) fn content_small(
        &self,
        ui: &mut egui::Ui,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        self.outlined(ui, text, egui::TextStyle::Small)
    }

    pub(crate) fn content_monospace(
        &self,
        ui: &mut egui::Ui,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        self.outlined(ui, text, egui::TextStyle::Monospace)
    }

    /// 中央滑杆（描边表头版）：复刻 `GlassSlider::show` 的布局/交互/绘制
    /// （头行左文 + 右值编辑 + 40 高玻璃滑块，`step` 同语义），
    /// 唯一差别是表头左文走描边——库内 `ui.label` 无描边可挂，
    /// 表头直接画在透明底图上，不描根本看不清（2026-10-21 故障）。
    /// 常数与库内同值（`CONTROL_HEIGHT 40 / THUMB 16 / ASPECT 0.625`），
    /// 库 pin 死（见根 `Cargo.toml`），变了这里同步跟。
    /// 关联函数（不借 `self`）：调用点多为 `&mut self` 方法内，
    /// 直接传 `self.dark` + `&mut self.x_f`，字段借用不打架。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn content_slider(
        ui: &mut egui::Ui,
        dark: bool,
        value: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        header: String,
        step: Option<f64>,
    ) -> egui::Response {
        let style = if dark {
            egui_glass::GlassStyle::dark()
        } else {
            egui_glass::GlassStyle::regular()
        };
        let old_value = *value;
        let width = ui.available_width().max(64.0);
        let mut response = ui
            .vertical(|ui| {
                ui.set_width(width);
                let header_resp = ui.horizontal(|ui| {
                    let label = Self::outlined_ui(ui, dark, header.clone(), egui::TextStyle::Body);
                    let v = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let span = (f64::from(*range.end()) - f64::from(*range.start())).abs();
                            ui.add(
                                egui::DragValue::new(value)
                                    .range(range.clone())
                                    .speed(step.unwrap_or(span / 200.0))
                                    .max_decimals(3),
                            )
                        })
                        .inner;
                    v.labelled_by(label.id)
                });
                let mut response = ui
                    .scope(|ui| {
                        ui.set_opacity(0.0);
                        ui.spacing_mut().slider_width = width;
                        ui.spacing_mut().interact_size.y = 40.0;
                        let mut slider = egui::Slider::new(value, range.clone())
                            .show_value(false)
                            .handle_shape(egui::style::HandleShape::Rect {
                                aspect_ratio: 0.625,
                            });
                        if let Some(st) = step {
                            slider = slider.step_by(st);
                        }
                        ui.add(slider)
                    })
                    .inner;
                response.widget_info(|| {
                    egui::WidgetInfo::slider(ui.is_enabled(), f64::from(*value), &header)
                });
                if ui.is_rect_visible(response.rect) {
                    Self::paint_slider_bar(ui, *value, &response, &range, style);
                }
                if header_resp.inner.changed() {
                    response.mark_changed();
                }
                response
            })
            .inner;
        if *value != old_value {
            response.mark_changed();
        }
        response
    }

    /// 滑块条绘制（库内 `paint` 同式，见上注释）。
    fn paint_slider_bar(
        ui: &egui::Ui,
        value: f32,
        response: &egui::Response,
        range: &std::ops::RangeInclusive<f32>,
        style: egui_glass::GlassStyle,
    ) {
        let rect = response.rect;
        let dark = ui.visuals().dark_mode;
        let accent = egui::Color32::from_rgb(0, 136, 255);
        let track_color = if dark {
            egui::Color32::from_white_alpha(45)
        } else {
            egui::Color32::from_black_alpha(26)
        };
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
        ui.painter().rect_filled(track, 3.0, track_color);
        let span = f64::from(*range.end()) - f64::from(*range.start());
        let fraction = if span == 0.0 {
            0.0
        } else {
            ((f64::from(value) - f64::from(*range.start())) / span).clamp(0.0, 1.0) as f32
        };
        let half_width = rect.height() / 2.5 * 0.625;
        let x = egui::lerp(
            (rect.left() + half_width)..=(rect.right() - half_width),
            fraction,
        );
        let thumb = egui::Rect::from_center_size(
            egui::pos2(x, rect.center().y),
            egui::vec2(half_width * 2.0, 16.0),
        );
        let fill = egui::Rect::from_min_max(track.min, egui::pos2(x, track.bottom()));
        ui.painter().rect_filled(
            fill,
            3.0,
            if ui.is_enabled() { accent } else { track_color },
        );
        if ui.is_enabled() {
            let style = if response.dragged() {
                style.pressed()
            } else if response.hovered() {
                style.hovered()
            } else {
                style
            };
            egui_glass::paint_glass(
                ui,
                thumb,
                &egui_glass::GlassStyle {
                    tint: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 245),
                    specular: style.specular.min(0.12),
                    border: style.border.min(0.08),
                    shadow: style.shadow.max(0.10),
                    shadow_radius: 6.0,
                    shadow_offset: 2.0,
                    ..style.capsule()
                },
            );
            ui.painter().rect_stroke(
                thumb,
                255.0,
                egui::Stroke::new(0.5, egui::Color32::from_white_alpha(180)),
                egui::StrokeKind::Inside,
            );
        } else {
            ui.painter()
                .rect_filled(thumb, 255.0, ui.visuals().widgets.noninteractive.bg_fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                thumb.expand(2.0),
                255.0,
                egui::Stroke::new(2.0, accent),
                egui::StrokeKind::Outside,
            );
        }
    }

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
        let col = if self.dark {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };
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
                    ui.selectable_value(&mut self.page, Page::Playlist, t!("nav.playlist"));
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
            // 图库选中 avc/hevc 片源即接管动态背景；无选中/非编解码回落图库首个
            // avc/hevc（默认跟随图库 mtime 最老的，不硬编码路径），都没有即静态底。
            // 会话级，无落盘（转码页状态本就不进 player.toml）。
            let want = crate::video_bg::VideoBg::resolve_source(
                &self.gallery.entries,
                self.gallery.selected,
                &crate::gallery::gallery_dir(&self.path),
            );
            self.video_bg.set_source(want);
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
                    let nav_fg = if self.dark {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::BLACK
                    };
                    ui.heading(egui::RichText::new(t!("nav.sections")).color(nav_fg));
                    self.nav_button(ui, Page::File, t!("nav.file").into_owned());
                    self.nav_button(ui, Page::Accel, t!("nav.accel").into_owned());
                    self.nav_button(ui, Page::Playlist, t!("nav.playlist").into_owned());
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
