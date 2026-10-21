//! 页面内容：`App::page_content`（八页开关）+ 件库约束见根 AGENTS（玻璃件/留白/无分隔线）。
//! 转码/图库两页实现在各自模块（同 `impl App`，跨文件只为守 1000 行上限）。

use eframe::egui;
use rust_i18n::t;

use crate::app::{App, DecodeTab, Page};
use crate::rules::*;
use crate::schema::*;
use crate::text::*;

impl App {
    /// 中央不透明面板里的开关（只拨开关，不暴露 TOML 原文）。
    pub(crate) fn page_content(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::File => {
                self.content_heading(ui, t!("file.heading"));
                self.content_small(ui, t!("file.hint"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    self.content_label(ui, t!("file.current"));
                    self.content_monospace(ui, self.path.as_str());
                });
                ui.add_space(4.0);
                self.content_small(ui, t!("file.tip"));
            }
            Page::Accel => {
                self.content_heading(ui, t!("accel.heading"));
                ui.add_space(4.0);
                self.content_label(ui, t!("accel.level"));
                // 档位滑杆：0-30 拨 `ALL` 下标，文本行同步显示当前档 + 说明。
                let cur = self.cfg.accel.level;
                let accel_label = format!("{}: {}", cur.file_str(), level_desc(cur));
                egui_glass::GlassSlider::new(&mut self.accel_f, 0.0..=30.0)
                    .text(accel_label)
                    .step_by(1.0)
                    .style(if self.dark {
                        egui_glass::GlassStyle::dark()
                    } else {
                        egui_glass::GlassStyle::regular()
                    })
                    .show(ui);
                self.cfg.accel.level =
                    AccelLevel::ALL[self.accel_f.round().clamp(0.0, 30.0) as usize];
                self.content_small(ui, t!("accel.auto_level"));
                ui.add_space(10.0);
                self.content_label(ui, t!("accel.mode"));
                ui.horizontal(|ui| {
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Auto,
                        &t!("accel.mode_name_auto"),
                    ) {
                        self.cfg.render.mode = RenderMode::Auto;
                    }
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Direct,
                        &t!("accel.mode_name_direct"),
                    ) {
                        self.cfg.render.mode = RenderMode::Direct;
                    }
                    if self.option_button(
                        ui,
                        self.cfg.render.mode == RenderMode::Blt,
                        &t!("accel.mode_name_blt"),
                    ) {
                        self.cfg.render.mode = RenderMode::Blt;
                    }
                });
                self.content_small(ui, mode_desc(self.cfg.render.mode));
                ui.add_space(10.0);
                if self.cfg.render.mode == RenderMode::Blt {
                    self.content_label(ui, t!("accel.blt_op"));
                    // 一行四个（窄窗自动换行保底，不断尾）。
                    // 只有 BufferToVideo 能上屏播视频，其余三项置灰展示、不删：
                    // 库内件无禁用态（`GlassButton::show` 写死 `Sense::click`，
                    // 自绘不跟随禁用样式），手动灰字 + `add_enabled_ui(false)` 关交互。
                    ui.horizontal_wrapped(|ui| {
                        for v in BltOpKind::ALL {
                            if v == BltOpKind::BufferToVideo {
                                if self.option_button(ui, self.cfg.render.blt_op == v, v.file_str())
                                {
                                    self.cfg.render.blt_op = v;
                                }
                            } else {
                                ui.add_enabled_ui(false, |ui| {
                                    egui_glass::GlassButton::new(format!(
                                        "{} {}",
                                        Self::marker(false),
                                        v.file_str()
                                    ))
                                    .text_color(egui::Color32::GRAY)
                                    .show(ui);
                                });
                            }
                        }
                    });
                    self.content_small(ui, blt_desc(self.cfg.render.blt_op));
                    ui.add_space(10.0);
                }
                // Blt 恒单线程：workers 整组不可选，直接隐藏（不摆开关也不留提示）。
                // backend 行常摆（nano+blt 合法：单核泵走 Blt 引擎；manual/dist 才看 render）。
                self.content_label(ui, t!("sched.backend"));
                ui.horizontal(|ui| {
                    for v in ScheduleBackend::ALL {
                        if self.option_button(ui, self.cfg.schedule.backend == v, v.file_str()) {
                            self.cfg.schedule.backend = v;
                        }
                    }
                });
                self.content_small(ui, sched_desc(self.cfg.schedule.backend));
                ui.add_space(10.0);
                if self.cfg.render.mode != RenderMode::Blt {
                    // manual 后端才用 workers/dist：执行器后端整组隐藏。
                    if self.cfg.schedule.backend == ScheduleBackend::Manual {
                        self.content_label(ui, t!("accel.workers"));
                        ui.horizontal(|ui| {
                            for v in WorkersMode::ALL {
                                if self.option_button(
                                    ui,
                                    self.cfg.schedule.workers == v,
                                    v.file_str(),
                                ) {
                                    self.cfg.schedule.workers = v;
                                }
                            }
                        });
                        self.content_small(ui, workers_desc(self.cfg.schedule.workers));
                        ui.add_space(10.0);
                        // cores 上限：bsp_only 钉死 1 核，整组隐藏（置灰不如隐藏，见壁纸/boot先例）。
                        if self.cfg.schedule.workers != WorkersMode::BspOnly {
                            self.content_label(ui, t!("sched.cores"));
                            let cores_label = format!(
                                "{}: {}",
                                t!("sched.cores"),
                                cores_text(self.cfg.schedule.cores)
                            );
                            egui_glass::GlassSlider::new(&mut self.sched_cores_f, 0.0..=64.0)
                                .text(cores_label)
                                .step_by(1.0)
                                .style(if self.dark {
                                    egui_glass::GlassStyle::dark()
                                } else {
                                    egui_glass::GlassStyle::regular()
                                })
                                .show(ui);
                            self.cfg.schedule.cores =
                                self.sched_cores_f.round().clamp(0.0, 64.0) as u32;
                            self.content_small(ui, cores_desc());
                            ui.add_space(10.0);
                        }
                        self.content_label(ui, t!("sched.roles"));
                        ui.horizontal(|ui| {
                            for v in RolesMode::ALL {
                                if self.option_button(
                                    ui,
                                    self.cfg.schedule.roles == v,
                                    v.file_str(),
                                ) {
                                    self.cfg.schedule.roles = v;
                                }
                            }
                        });
                        self.content_small(ui, roles_desc(self.cfg.schedule.roles));
                        ui.add_space(10.0);
                        // schedule sweep 记忆只读行（`cores n/m`，与固件 `sched: crew=` 同口径；
                        // 清零走下方统一重测按钮，不在此单设按钮）。
                        self.content_label(ui, self.schedule_measured_line());
                        ui.add_space(10.0);
                        // dist 只在直写 + 全核时有意义：单核下整组隐藏。
                        if self.cfg.schedule.workers != WorkersMode::BspOnly {
                            self.content_label(ui, t!("accel.dist"));
                            // 一行四个（窄窗自动换行保底，不断尾；与 blt 行同式）。
                            ui.horizontal_wrapped(|ui| {
                                for v in DistMode::ALL {
                                    if self.option_button(
                                        ui,
                                        self.cfg.schedule.dist == v,
                                        v.file_str(),
                                    ) {
                                        self.cfg.schedule.dist = v;
                                    }
                                }
                            });
                            self.content_small(ui, dist_desc(self.cfg.schedule.dist));
                            ui.add_space(10.0);
                        }
                    }
                }
                self.content_label(ui, t!("accel.vsync"));
                // 一行四个（窄窗自动换行保底，不断尾；与 dist 行同式）。
                ui.horizontal_wrapped(|ui| {
                    for v in VsyncMode::ALL {
                        if self.option_button(ui, self.cfg.render.vsync == v, v.file_str()) {
                            self.cfg.render.vsync = v;
                        }
                    }
                });
                self.content_small(ui, vsync_desc(self.cfg.render.vsync));
                // half 实测行：有值报预算，无值提示下次启动现测取半 + 重测按钮。
                if self.cfg.render.vsync == VsyncMode::Half {
                    ui.horizontal(|ui| {
                        self.content_label(ui, self.half_line());
                        if egui_glass::GlassButton::new(t!("vsync.half_rebench"))
                            .show(ui)
                            .clicked()
                        {
                            // 清零并存盘：下次启动 half 会重测。
                            self.cfg.render.measured_vsync_fps = 0;
                            let p = self.path.clone();
                            self.save_to(&p);
                        }
                    });
                }
                ui.add_space(10.0);
                // 微调只摆本模式用得上的，整组无关直接隐藏（判定见 `tune_visible`）。
                let vis = tune_visible(&self.cfg);
                if vis.frames || vis.tile || vis.vfps || vis.buf {
                    self.content_label(ui, t!("tune.heading"));
                    self.content_small(ui, t!("tune.hint"));
                    if vis.frames {
                        self.content_label(ui, t!("tune.frames"));
                        let frames_label = format!("{} frames", self.cfg.render.tune.frames);
                        egui_glass::GlassSlider::new(&mut self.tune_frames_f, 1.0..=120.0)
                            .text(frames_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.frames =
                            self.tune_frames_f.round().clamp(1.0, 120.0) as u32;
                    }
                    if vis.tile {
                        self.content_label(ui, t!("tune.tile_rows"));
                        let tile_label = format!("{} rows", self.cfg.render.tune.tile_rows);
                        egui_glass::GlassSlider::new(&mut self.tune_tile_f, 8.0..=512.0)
                            .text(tile_label)
                            .step_by(8.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.tile_rows =
                            self.tune_tile_f.round().clamp(8.0, 512.0) as u32;
                    }
                    if vis.vfps {
                        self.content_label(ui, t!("tune.vsync_fps"));
                        let vsync_fps_label = format!("{} fps", self.cfg.render.tune.vsync_fps);
                        egui_glass::GlassSlider::new(&mut self.tune_vsync_fps_f, 15.0..=240.0)
                            .text(vsync_fps_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.vsync_fps =
                            self.tune_vsync_fps_f.round().clamp(15.0, 240.0) as u32;
                    }
                    if vis.buf {
                        self.content_label(ui, t!("tune.buffers"));
                        let buffers_label = if self.cfg.render.tune.buffers == 0 {
                            t!("tune.buffers_off").into_owned()
                        } else {
                            format!("{} buffers", self.cfg.render.tune.buffers)
                        };
                        egui_glass::GlassSlider::new(&mut self.tune_buffers_f, 0.0..=16.0)
                            .text(buffers_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.render.tune.buffers =
                            self.tune_buffers_f.round().clamp(0.0, 16.0) as u32;
                    }
                    self.content_small(ui, t!("tune.note"));
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    self.content_label(ui, self.render_measured_line());
                    if egui_glass::GlassButton::new(t!("render.rebench"))
                        .show(ui)
                        .clicked()
                    {
                        // 清零记忆并存盘：任一 auto 下次启动重校准。
                        // half 预算派生自矩阵计时，一并清零。
                        // schedule sweep 胜者同理清零（只读展示，下次启动重 sweep）。
                        self.cfg.render.measured_render = RenderMode::Auto;
                        self.cfg.render.measured_workers = WorkersMode::Auto;
                        self.cfg.render.measured_dist = DistMode::Auto;
                        self.cfg.render.measured_vsync = VsyncMode::Auto;
                        self.cfg.render.measured_vsync_fps = 0;
                        self.cfg.render.measured_cores = 0;
                        self.cfg.schedule.measured_workers_n = 0;
                        let p = self.path.clone();
                        self.save_to(&p);
                    }
                });
                ui.add_space(10.0);
                self.content_label(ui, t!("async.executor"));
                ui.horizontal(|ui| {
                    for v in AsyncExecutor::ALL {
                        if self.option_button(ui, self.cfg.r#async.executor == v, v.file_str()) {
                            self.cfg.r#async.executor = v;
                        }
                    }
                });
                self.content_small(ui, async_desc(self.cfg.r#async.executor));
            }
            Page::Playlist => {
                self.playlist_page(ui);
            }
            Page::Decode => {
                self.content_heading(ui, t!("decode.heading"));
                self.content_small(ui, t!("decode.hint"));
                ui.add_space(4.0);
                // tab 条已抽到卡片顶部固定（`App::decode_tab_bar`，滚动区之外）；
                // 这里只画选中 tab 的内容。全局默认（播放帧率 + 供片）走 `Global` tab。
                match self.decode_tab {
                    DecodeTab::Global => {
                        // 全局默认（`Global` tab）：播放帧率 + 供片。
                        // `[play] feed` 是全局默认，逐格式 `fetch` 覆盖它（`None` = 继承）；
                        // 壁纸恒 `clip`，忽略覆盖。
                        self.content_label(ui, t!("decode.global_heading"));
                        self.content_small(ui, t!("decode.global_hint"));
                        ui.add_space(4.0);
                        self.content_label(ui, t!("play.fps"));
                        // 三段滑杆：0 = auto，1-240 = 限速，241 = max。
                        // 文本先算好：滑杆拿 &mut play_f 时不能再借 self。
                        let play_label = self.play_text();
                        egui_glass::GlassSlider::new(&mut self.play_f, 0.0..=241.0)
                            .text(play_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        let pos = self.play_f.round().clamp(0.0, 241.0) as u32;
                        self.cfg.play.fps = match pos {
                            0 => String::from("auto"),
                            241 => String::from("max"),
                            n => n.to_string(),
                        };
                        self.content_small(ui, t!("play.hint"));
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            self.content_label(ui, self.measured_line());
                            if egui_glass::GlassButton::new(t!("play.rebench"))
                                .show(ui)
                                .clicked()
                            {
                                // 清零并存盘：下次启动 auto 会重测。
                                self.cfg.play.measured_max_fps = 0;
                                let p = self.path.clone();
                                self.save_to(&p);
                            }
                        });
                        ui.add_space(4.0);
                        self.content_label(ui, t!("play.feed"));
                        // 供片滑杆：0-4 拨 `FeedMode::ALL` 下标，文本行同步显示当前档 + 说明。
                        let cur_feed = self.cfg.play.feed;
                        let feed_label = format!(
                            "{}: {}",
                            cur_feed.file_str(),
                            crate::text::feed_desc(cur_feed)
                        );
                        egui_glass::GlassSlider::new(&mut self.feed_f, 0.0..=4.0)
                            .text(feed_label)
                            .step_by(1.0)
                            .style(if self.dark {
                                egui_glass::GlassStyle::dark()
                            } else {
                                egui_glass::GlassStyle::regular()
                            })
                            .show(ui);
                        self.cfg.play.feed =
                            FeedMode::ALL[self.feed_f.round().clamp(0.0, 4.0) as usize];
                        self.content_small(ui, t!("play.feed_hint"));
                    }
                    DecodeTab::Format(fmt) => {
                        // 选中 tab 的 5 控件（线程滑杆 + 零拷开关 + 流式开关 + 缓冲滑杆 +
                        // 供片覆盖滑杆；八格式全表，`qois` 帧级并行可兑现、`raw` 零解码占位）。
                        {
                            let i = DecodeTable::ORDER
                                .iter()
                                .position(|&f| f == fmt)
                                .unwrap_or(0);
                            self.content_label(ui, format_name(fmt));
                            self.content_small(ui, format_desc(fmt));
                            // 线程：0-1 拨 `DecodeThreads::ALL` 下标。
                            self.content_label(ui, t!("decode.threads"));
                            let cur_t = self
                                .cfg
                                .decode
                                .get(fmt)
                                .map(|d| d.threads)
                                .unwrap_or_default();
                            let threads_label =
                                format!("{}: {}", cur_t.file_str(), decode_threads_desc(cur_t));
                            egui_glass::GlassSlider::new(&mut self.dec_threads_f[i], 0.0..=1.0)
                                .text(threads_label)
                                .step_by(1.0)
                                .style(if self.dark {
                                    egui_glass::GlassStyle::dark()
                                } else {
                                    egui_glass::GlassStyle::regular()
                                })
                                .show(ui);
                            if let Some(d) = self.cfg.decode.get_mut(fmt) {
                                d.threads = DecodeThreads::ALL
                                    [self.dec_threads_f[i].round().clamp(0.0, 1.0) as usize];
                            }
                            self.content_small(ui, t!("decode.threads_hint"));
                            ui.add_space(4.0);
                            // 零拷：两颗玻璃按钮单选开关。
                            self.content_label(ui, t!("decode.zero_copy"));
                            let zc = self.switch_row(
                                ui,
                                self.cfg
                                    .decode
                                    .get(fmt)
                                    .map(|d| d.zero_copy)
                                    .unwrap_or(false),
                                &t!("boot.on"),
                                &t!("boot.off"),
                            );
                            if let Some(d) = self.cfg.decode.get_mut(fmt) {
                                d.zero_copy = zc;
                            }
                            self.content_small(ui, t!("decode.desc_zero_copy"));
                            ui.add_space(4.0);
                            // 流式：同上单选开关。
                            self.content_label(ui, t!("decode.streaming"));
                            let st = self.switch_row(
                                ui,
                                self.cfg
                                    .decode
                                    .get(fmt)
                                    .map(|d| d.streaming)
                                    .unwrap_or(false),
                                &t!("boot.on"),
                                &t!("boot.off"),
                            );
                            if let Some(d) = self.cfg.decode.get_mut(fmt) {
                                d.streaming = st;
                            }
                            self.content_small(ui, t!("decode.desc_streaming"));
                            ui.add_space(4.0);
                            // 缓冲：0 = 跟随 `tune.window`，1-32 显式按格式覆盖。
                            self.content_label(ui, t!("decode.buffers"));
                            let cur_b = self
                                .cfg
                                .decode
                                .get(fmt)
                                .map(|d| d.buffers.min(32))
                                .unwrap_or(0);
                            let buffers_label = if cur_b == 0 {
                                t!("decode.buffers_follow", v = self.cfg.render.tune.window)
                                    .into_owned()
                            } else {
                                format!("{} buffers", cur_b)
                            };
                            egui_glass::GlassSlider::new(&mut self.dec_buffers_f[i], 0.0..=32.0)
                                .text(buffers_label)
                                .step_by(1.0)
                                .style(if self.dark {
                                    egui_glass::GlassStyle::dark()
                                } else {
                                    egui_glass::GlassStyle::regular()
                                })
                                .show(ui);
                            if let Some(d) = self.cfg.decode.get_mut(fmt) {
                                d.buffers = self.dec_buffers_f[i].round().clamp(0.0, 32.0) as u32;
                            }
                            self.content_small(ui, t!("decode.buffers_hint"));
                            ui.add_space(4.0);
                            // 供片覆盖：0 = 继承全局 `[play] feed`（落盘不写键），1-5 显式五档。
                            // 行标题复用 `play.feed`，档位文案复用 `feed.desc_*`，继承文案走
                            // `decode.fetch_inherit`（唯一新增 locale 键，中英对称）。
                            self.content_label(ui, t!("play.feed"));
                            let cur_fetch = self.cfg.decode.get(fmt).and_then(|d| d.fetch);
                            let fetch_label = decode_fetch_desc(cur_fetch);
                            egui_glass::GlassSlider::new(&mut self.dec_fetch_f[i], 0.0..=5.0)
                                .text(fetch_label)
                                .step_by(1.0)
                                .style(if self.dark {
                                    egui_glass::GlassStyle::dark()
                                } else {
                                    egui_glass::GlassStyle::regular()
                                })
                                .show(ui);
                            let pos = self.dec_fetch_f[i].round().clamp(0.0, 5.0) as usize;
                            if let Some(d) = self.cfg.decode.get_mut(fmt) {
                                d.fetch = if pos == 0 {
                                    None
                                } else {
                                    Some(FeedMode::ALL[pos - 1])
                                };
                            }
                            self.content_small(ui, t!("play.feed_hint"));
                        }
                    }
                }
            }
            Page::Wallpaper => {
                self.content_heading(ui, t!("wallpaper.heading"));
                self.content_small(ui, t!("wallpaper.hint"));
                ui.add_space(4.0);
                self.content_label(ui, t!("wallpaper.enable"));
                ui.horizontal(|ui| {
                    if self.option_button(ui, self.cfg.wallpaper.enable, &t!("wallpaper.on")) {
                        self.cfg.wallpaper.enable = true;
                    }
                    if self.option_button(ui, !self.cfg.wallpaper.enable, &t!("wallpaper.off")) {
                        self.cfg.wallpaper.enable = false;
                    }
                });
                self.content_small(ui, wp_enable_desc(self.cfg.wallpaper.enable));
                // 关壁纸时循环/帧率/预载/冻结/列表全不可选，整组隐藏。
                // 顺序与 `[wallpaper]` 表一致：loop → fps → preload → popup_freeze → items。
                if self.cfg.wallpaper.enable {
                    ui.add_space(10.0);
                    // 循环（`playlist.rs`，与播放列表同编辑器）。
                    self.wallpaper_loop_row(ui);
                    ui.add_space(10.0);
                    self.content_label(ui, t!("wallpaper.fps"));
                    // 三段滑杆（与播放帧率同构）：0 = auto（跟 vsync），1-240 = 限速，241 = max。
                    let fps_label = self.wp_fps_text();
                    egui_glass::GlassSlider::new(&mut self.wp_fps_f, 0.0..=241.0)
                        .text(fps_label)
                        .step_by(1.0)
                        .style(if self.dark {
                            egui_glass::GlassStyle::dark()
                        } else {
                            egui_glass::GlassStyle::regular()
                        })
                        .show(ui);
                    let pos = self.wp_fps_f.round().clamp(0.0, 241.0) as u32;
                    self.cfg.wallpaper.fps = match pos {
                        0 => String::from("auto"),
                        241 => String::from("max"),
                        n => n.to_string(),
                    };
                    self.content_small(ui, t!("wallpaper.fps_hint"));
                    ui.add_space(10.0);
                    self.content_label(ui, t!("wallpaper.preload"));
                    // 最左 0 = 不封顶（逐条目截到 `preload`，0 = uncapped），1-32 封顶帧数。
                    let preload_label = if self.cfg.wallpaper.preload == 0 {
                        t!("wallpaper.preload_full").into_owned()
                    } else {
                        format!("{} frames", self.cfg.wallpaper.preload)
                    };
                    egui_glass::GlassSlider::new(&mut self.wp_preload_f, 0.0..=32.0)
                        .text(preload_label)
                        .step_by(1.0)
                        .style(if self.dark {
                            egui_glass::GlassStyle::dark()
                        } else {
                            egui_glass::GlassStyle::regular()
                        })
                        .show(ui);
                    self.cfg.wallpaper.preload = self.wp_preload_f.round().clamp(0.0, 32.0) as u32;
                    self.content_small(ui, t!("wallpaper.preload_hint"));
                    ui.add_space(10.0);
                    self.content_label(ui, t!("wallpaper.popup_freeze"));
                    let froze = self.switch_row(
                        ui,
                        self.cfg.wallpaper.popup_freeze,
                        &t!("boot.on"),
                        &t!("boot.off"),
                    );
                    self.cfg.wallpaper.popup_freeze = froze;
                    self.content_small(ui, t!("wallpaper.popup_freeze_hint"));
                    ui.add_space(10.0);
                    self.content_label(ui, t!("wallpaper.list_heading"));
                    self.wallpaper_items_editor(ui);
                }
            }
            Page::Boot => {
                self.content_heading(ui, t!("boot.heading"));
                self.content_small(ui, t!("boot.hint"));
                ui.add_space(4.0);
                self.content_label(ui, t!("boot.auto"));
                let auto = self.switch_row(ui, self.cfg.boot.auto, &t!("boot.on"), &t!("boot.off"));
                self.cfg.boot.auto = auto;
                self.content_small(ui, t!("boot.auto_desc"));
                // 总开关关时细项不可选，整组隐藏（壁纸页同式）。
                if self.cfg.boot.auto {
                    ui.add_space(10.0);
                    self.content_label(ui, t!("boot.windows"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.windows, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.windows = v;
                    ui.add_space(4.0);
                    self.content_label(ui, t!("boot.linux"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.linux, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.linux = v;
                    ui.add_space(4.0);
                    self.content_label(ui, t!("boot.shell"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.shell, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.shell = v;
                    ui.add_space(4.0);
                    self.content_label(ui, t!("boot.uki"));
                    let v = self.switch_row(ui, self.cfg.boot.uki, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.uki = v;
                    ui.add_space(4.0);
                    self.content_label(ui, t!("boot.macos"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.macos, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.macos = v;
                    ui.add_space(4.0);
                    self.content_label(ui, t!("boot.strict"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.strict, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.strict = v;
                }
            }
            Page::Transcode => {
                self.transcode_page(ui);
            }
            Page::Gallery => {
                self.gallery_page(ui);
            }
            Page::Summary => {
                self.content_heading(ui, t!("sum.heading"));
                self.content_small(ui, t!("sum.hint"));
                ui.add_space(8.0);
                self.content_label(ui, t!("sum.accel", v = self.cfg.accel.level.file_str()));
                self.content_label(ui, t!("sum.render", v = self.cfg.render.mode.file_str()));
                if self.cfg.render.mode == RenderMode::Blt {
                    self.content_label(ui, t!("sum.blt", v = self.cfg.render.blt_op.file_str()));
                }
                self.content_label(
                    ui,
                    t!("sum.workers", v = self.cfg.schedule.workers.file_str()),
                );
                self.content_label(
                    ui,
                    t!(
                        "sum.cores",
                        v = if self.cfg.schedule.cores == 0 {
                            String::from("auto")
                        } else {
                            self.cfg.schedule.cores.min(64).to_string()
                        }
                    ),
                );
                self.content_label(ui, t!("sum.roles", v = self.cfg.schedule.roles.file_str()));
                self.content_label(ui, t!("sum.dist", v = self.cfg.schedule.dist.file_str()));
                self.content_label(
                    ui,
                    t!("sum.backend", v = self.cfg.schedule.backend.file_str()),
                );
                // 固件串口同式（仅展示）：`sched: crew=<n>/<m> roles=<r> workers=<w>`。
                self.content_label(ui, self.sched_crew_line());
                self.content_label(ui, self.schedule_measured_line());
                self.content_label(ui, t!("sum.vsync", v = self.cfg.render.vsync.file_str()));
                self.content_label(ui, self.render_measured_line());
                self.content_label(ui, t!("sum.wallpaper", v = self.wallpaper_text()));
                self.content_label(
                    ui,
                    t!(
                        "sum.playlist",
                        v = format!(
                            "{} items loop={}",
                            self.cfg.playlist.items.len(),
                            self.cfg.playlist.loop_mode.file_str(),
                        )
                    ),
                );
                self.content_label(ui, t!("sum.play", v = self.play_text()));
                self.content_label(
                    ui,
                    t!("sum.async", v = self.cfg.r#async.executor.file_str()),
                );
                self.content_label(ui, self.measured_line());
                self.content_label(
                    ui,
                    t!(
                        "sum.boot",
                        v = if self.cfg.boot.auto {
                            format!(
                                "auto win:{} lin:{} sh:{} uki:{} mac:{} st:{}",
                                self.cfg.boot.windows as u8,
                                self.cfg.boot.linux as u8,
                                self.cfg.boot.shell as u8,
                                self.cfg.boot.uki as u8,
                                self.cfg.boot.macos as u8,
                                self.cfg.boot.strict as u8,
                            )
                        } else {
                            String::from("off")
                        }
                    ),
                );
                self.content_label(ui, t!("sum.version", v = self.cfg.version));
                self.content_label(ui, t!("sum.file", v = self.path.clone()));
                ui.add_space(8.0);
                if egui_glass::GlassButton::new(t!("toolbar.save"))
                    .show(ui)
                    .clicked()
                {
                    let p = self.path.clone();
                    self.save_to(&p);
                }
            }
        }
    }
}
