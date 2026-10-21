//! 页面内容：`App::page_content`（八页开关）+ 件库约束见根 AGENTS（玻璃件/留白/无分隔线）。
//! 转码/图库两页实现在各自模块（同 `impl App`，跨文件只为守 1000 行上限）。

use eframe::egui;
use rust_i18n::t;

use crate::app::{App, Page};
use crate::rules::*;
use crate::schema::*;
use crate::text::*;

impl App {
    /// 中央不透明面板里的开关（只拨开关，不暴露 TOML 原文）。
    pub(crate) fn page_content(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::File => {
                ui.heading(t!("file.heading"));
                ui.small(t!("file.hint"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(t!("file.current"));
                    ui.monospace(self.path.as_str());
                });
                ui.add_space(4.0);
                ui.small(t!("file.tip"));
            }
            Page::Accel => {
                ui.heading(t!("accel.heading"));
                ui.add_space(4.0);
                ui.label(t!("accel.level"));
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
                ui.small(t!("accel.auto_level"));
                ui.add_space(10.0);
                ui.label(t!("accel.mode"));
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
                ui.small(mode_desc(self.cfg.render.mode));
                ui.add_space(10.0);
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.label(t!("accel.blt_op"));
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
                    ui.small(blt_desc(self.cfg.render.blt_op));
                    ui.add_space(10.0);
                }
                // Blt 恒单线程：workers 整组不可选，直接隐藏（不摆开关也不留提示）。
                // backend 行常摆（nano+blt 合法：单核泵走 Blt 引擎；manual/dist 才看 render）。
                ui.label(t!("sched.backend"));
                ui.horizontal(|ui| {
                    for v in ScheduleBackend::ALL {
                        if self.option_button(ui, self.cfg.schedule.backend == v, v.file_str()) {
                            self.cfg.schedule.backend = v;
                        }
                    }
                });
                ui.small(sched_desc(self.cfg.schedule.backend));
                ui.add_space(10.0);
                if self.cfg.render.mode != RenderMode::Blt {
                    // manual 后端才用 workers/dist：执行器后端整组隐藏。
                    if self.cfg.schedule.backend == ScheduleBackend::Manual {
                        ui.label(t!("accel.workers"));
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
                        ui.small(workers_desc(self.cfg.schedule.workers));
                        ui.add_space(10.0);
                        // dist 只在直写 + 全核时有意义：单核下整组隐藏。
                        if self.cfg.schedule.workers != WorkersMode::BspOnly {
                            ui.label(t!("accel.dist"));
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
                            ui.small(dist_desc(self.cfg.schedule.dist));
                            ui.add_space(10.0);
                        }
                    }
                }
                ui.label(t!("accel.vsync"));
                // 一行四个（窄窗自动换行保底，不断尾；与 dist 行同式）。
                ui.horizontal_wrapped(|ui| {
                    for v in VsyncMode::ALL {
                        if self.option_button(ui, self.cfg.render.vsync == v, v.file_str()) {
                            self.cfg.render.vsync = v;
                        }
                    }
                });
                ui.small(vsync_desc(self.cfg.render.vsync));
                // half 实测行：有值报预算，无值提示下次启动现测取半 + 重测按钮。
                if self.cfg.render.vsync == VsyncMode::Half {
                    ui.horizontal(|ui| {
                        ui.label(self.half_line());
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
                    ui.label(t!("tune.heading"));
                    ui.small(t!("tune.hint"));
                    if vis.frames {
                        ui.label(t!("tune.frames"));
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
                        ui.label(t!("tune.tile_rows"));
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
                        ui.label(t!("tune.vsync_fps"));
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
                        ui.label(t!("tune.buffers"));
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
                    ui.small(t!("tune.note"));
                    ui.add_space(10.0);
                }
                ui.horizontal(|ui| {
                    ui.label(self.render_measured_line());
                    if egui_glass::GlassButton::new(t!("render.rebench"))
                        .show(ui)
                        .clicked()
                    {
                        // 清零记忆并存盘：任一 auto 下次启动重校准。
                        // half 预算派生自矩阵计时，一并清零。
                        self.cfg.render.measured_render = RenderMode::Auto;
                        self.cfg.render.measured_workers = WorkersMode::Auto;
                        self.cfg.render.measured_dist = DistMode::Auto;
                        self.cfg.render.measured_vsync = VsyncMode::Auto;
                        self.cfg.render.measured_vsync_fps = 0;
                        self.cfg.render.measured_cores = 0;
                        let p = self.path.clone();
                        self.save_to(&p);
                    }
                });
                ui.add_space(10.0);
                ui.label(t!("async.executor"));
                ui.horizontal(|ui| {
                    for v in AsyncExecutor::ALL {
                        if self.option_button(ui, self.cfg.r#async.executor == v, v.file_str()) {
                            self.cfg.r#async.executor = v;
                        }
                    }
                });
                ui.small(async_desc(self.cfg.r#async.executor));
            }
            Page::Playlist => {
                self.playlist_page(ui);
            }
            Page::Decode => {
                ui.heading(t!("decode.heading"));
                ui.small(t!("decode.hint"));
                ui.add_space(4.0);
                // 全局默认（家在解码页顶）：播放帧率 + 供片。
                // `[play] feed` 是全局默认，逐格式 `fetch` 覆盖它（`None` = 继承）；
                // 壁纸恒 `clip`，忽略覆盖。
                ui.label(t!("decode.global_heading"));
                ui.small(t!("decode.global_hint"));
                ui.add_space(4.0);
                ui.label(t!("play.fps"));
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
                ui.small(t!("play.hint"));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(self.measured_line());
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
                ui.label(t!("play.feed"));
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
                self.cfg.play.feed = FeedMode::ALL[self.feed_f.round().clamp(0.0, 4.0) as usize];
                ui.small(t!("play.feed_hint"));
                ui.add_space(10.0);
                // 8 格式 tabs（GlassButton 顶栏，选中描蓝 + ●；只切展示，不落盘）。
                ui.label(t!("decode.tabs_heading"));
                ui.small(t!("decode.tabs_hint"));
                ui.horizontal_wrapped(|ui| {
                    for fmt in DecodeTable::ORDER.iter().copied() {
                        let selected = self.decode_tab == fmt;
                        if self.option_button(ui, selected, &format_name(fmt)) {
                            self.decode_tab = fmt;
                        }
                    }
                });
                ui.add_space(4.0);
                // 选中 tab 的 5 控件（线程滑杆 + 零拷开关 + 流式开关 + 缓冲滑杆 +
                // 供片覆盖滑杆；八格式全表，`qois` 帧级并行可兑现、`raw` 零解码占位）。
                {
                    let fmt = self.decode_tab;
                    let i = DecodeTable::ORDER
                        .iter()
                        .position(|&f| f == fmt)
                        .unwrap_or(0);
                    ui.label(format_name(fmt));
                    ui.small(format_desc(fmt));
                    // 线程：0-1 拨 `DecodeThreads::ALL` 下标。
                    ui.label(t!("decode.threads"));
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
                    ui.small(t!("decode.threads_hint"));
                    ui.add_space(4.0);
                    // 零拷：两颗玻璃按钮单选开关。
                    ui.label(t!("decode.zero_copy"));
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
                    ui.small(t!("decode.desc_zero_copy"));
                    ui.add_space(4.0);
                    // 流式：同上单选开关。
                    ui.label(t!("decode.streaming"));
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
                    ui.small(t!("decode.desc_streaming"));
                    ui.add_space(4.0);
                    // 缓冲：0 = 跟随 `tune.window`，1-32 显式按格式覆盖。
                    ui.label(t!("decode.buffers"));
                    let cur_b = self
                        .cfg
                        .decode
                        .get(fmt)
                        .map(|d| d.buffers.min(32))
                        .unwrap_or(0);
                    let buffers_label = if cur_b == 0 {
                        t!("decode.buffers_follow", v = self.cfg.render.tune.window).into_owned()
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
                    ui.small(t!("decode.buffers_hint"));
                    ui.add_space(4.0);
                    // 供片覆盖：0 = 继承全局 `[play] feed`（落盘不写键），1-5 显式五档。
                    // 行标题复用 `play.feed`，档位文案复用 `feed.desc_*`，继承文案走
                    // `decode.fetch_inherit`（唯一新增 locale 键，中英对称）。
                    ui.label(t!("play.feed"));
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
                    ui.small(t!("play.feed_hint"));
                }
            }
            Page::Wallpaper => {
                ui.heading(t!("wallpaper.heading"));
                ui.small(t!("wallpaper.hint"));
                ui.add_space(4.0);
                ui.label(t!("wallpaper.enable"));
                ui.horizontal(|ui| {
                    if self.option_button(ui, self.cfg.wallpaper.enable, &t!("wallpaper.on")) {
                        self.cfg.wallpaper.enable = true;
                    }
                    if self.option_button(ui, !self.cfg.wallpaper.enable, &t!("wallpaper.off")) {
                        self.cfg.wallpaper.enable = false;
                    }
                });
                ui.small(wp_enable_desc(self.cfg.wallpaper.enable));
                // 关壁纸时循环/帧率/预载/冻结/列表全不可选，整组隐藏。
                // 顺序与 `[wallpaper]` 表一致：loop → fps → preload → popup_freeze → items。
                if self.cfg.wallpaper.enable {
                    ui.add_space(10.0);
                    // 循环（`playlist.rs`，与播放列表同编辑器）。
                    self.wallpaper_loop_row(ui);
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.fps"));
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
                    ui.small(t!("wallpaper.fps_hint"));
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.preload"));
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
                    ui.small(t!("wallpaper.preload_hint"));
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.popup_freeze"));
                    let froze = self.switch_row(
                        ui,
                        self.cfg.wallpaper.popup_freeze,
                        &t!("boot.on"),
                        &t!("boot.off"),
                    );
                    self.cfg.wallpaper.popup_freeze = froze;
                    ui.small(t!("wallpaper.popup_freeze_hint"));
                    ui.add_space(10.0);
                    ui.label(t!("wallpaper.list_heading"));
                    self.wallpaper_items_editor(ui);
                }
            }
            Page::Boot => {
                ui.heading(t!("boot.heading"));
                ui.small(t!("boot.hint"));
                ui.add_space(4.0);
                ui.label(t!("boot.auto"));
                let auto = self.switch_row(ui, self.cfg.boot.auto, &t!("boot.on"), &t!("boot.off"));
                self.cfg.boot.auto = auto;
                ui.small(t!("boot.auto_desc"));
                // 总开关关时细项不可选，整组隐藏（壁纸页同式）。
                if self.cfg.boot.auto {
                    ui.add_space(10.0);
                    ui.label(t!("boot.windows"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.windows, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.windows = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.linux"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.linux, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.linux = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.shell"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.shell, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.shell = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.uki"));
                    let v = self.switch_row(ui, self.cfg.boot.uki, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.uki = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.macos"));
                    let v =
                        self.switch_row(ui, self.cfg.boot.macos, &t!("boot.on"), &t!("boot.off"));
                    self.cfg.boot.macos = v;
                    ui.add_space(4.0);
                    ui.label(t!("boot.strict"));
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
                ui.heading(t!("sum.heading"));
                ui.small(t!("sum.hint"));
                ui.add_space(8.0);
                ui.label(t!("sum.accel", v = self.cfg.accel.level.file_str()));
                ui.label(t!("sum.render", v = self.cfg.render.mode.file_str()));
                if self.cfg.render.mode == RenderMode::Blt {
                    ui.label(t!("sum.blt", v = self.cfg.render.blt_op.file_str()));
                }
                ui.label(t!("sum.workers", v = self.cfg.schedule.workers.file_str()));
                ui.label(t!("sum.dist", v = self.cfg.schedule.dist.file_str()));
                ui.label(t!("sum.backend", v = self.cfg.schedule.backend.file_str()));
                ui.label(t!("sum.vsync", v = self.cfg.render.vsync.file_str()));
                ui.label(self.render_measured_line());
                ui.label(t!("sum.wallpaper", v = self.wallpaper_text()));
                ui.label(t!(
                    "sum.playlist",
                    v = format!(
                        "{} items loop={}",
                        self.cfg.playlist.items.len(),
                        self.cfg.playlist.loop_mode.file_str(),
                    )
                ));
                ui.label(t!("sum.play", v = self.play_text()));
                ui.label(t!("sum.async", v = self.cfg.r#async.executor.file_str()));
                ui.label(self.measured_line());
                ui.label(t!(
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
                ));
                ui.label(t!("sum.version", v = self.cfg.version));
                ui.label(t!("sum.file", v = self.path.clone()));
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
