//! 转码页 UI（`impl App`；拆自 `transcode.rs`，行为零改）。

use super::*;

impl App {
    /// 选源后立即探针（同步，快）：行内直接看到“是什么片子”，不抽象。
    pub(crate) fn refresh_probe(&mut self) {
        if self.transcode.source.is_empty() {
            self.transcode.info.clear();
            return;
        }
        match probe_source(&self.transcode.source) {
            Ok(info) => {
                // 选片即回填源几何（用户拍板：默认跟源，不放大；手动改完换片会被新源覆盖）。
                self.transcode.res_text = format!("{}x{}", info.w, info.h);
                self.transcode.info = info.summary();
                self.transcode.status.clear();
            }
            Err(e) => {
                self.transcode.info.clear();
                self.transcode.status = format!("probe: {e}");
            }
        }
    }

    /// 转码页：源行 + 探针行 + 目标行 + 输出行 + 编码按钮 + 状态。
    pub(crate) fn transcode_page(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        // rfd 收果（后台线程框选完落袋；见 `file_pick`，winit 重入 abort 血泪）。
        if let Some(f) = self.transcode.pick.take().flatten() {
            self.transcode.source = f.display().to_string();
            self.refresh_probe();
        }
        use rust_i18n::t;
        // 先收 worker 进度（UI 线程只 try_recv，不阻塞）。
        self.poll_transcode_job();
        // 任务跑着就一直要帧：eframe 按需重绘，不主动要就永远停在
        // 点击那一帧，进度/完成/报错全都刷不出来（“0/0 卡死”的真凶之一）。
        if self.transcode.running() {
            ui.ctx().request_repaint();
        }
        self.content_heading(ui, t!("transcode.heading"));
        self.content_small(ui, t!("transcode.hint"));
        self.content_small(ui, t!("transcode.external_hint"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            self.content_label(ui, t!("transcode.source"));
            let shown = if self.transcode.source.is_empty() {
                t!("transcode.no_source").into_owned()
            } else {
                self.transcode.source.clone()
            };
            self.content_monospace(ui, shown);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if egui_glass::GlassButton::new(t!("transcode.pick"))
                .show(ui)
                .clicked()
            {
                // 后台线程跑阻塞框（winit 重入 abort 血泪，见 `file_pick`）；
                // 框开着时不再起新框。
                if !self.transcode.pick.pending() {
                    let dir = crate::gallery::gallery_dir(&self.path);
                    self.transcode.pick.open(move || {
                        rfd::FileDialog::new()
                            .set_directory(dir)
                            .add_filter(
                                "video/image",
                                &["mp4", "png", "jpg", "jpeg", "bmp", "raw", "raws"],
                            )
                            .pick_file()
                    });
                }
            }
            if egui_glass::GlassButton::new(t!("transcode.example"))
                .show(ui)
                .clicked()
            {
                self.transcode.source = String::from("/Users/bemly/Movies/input.mp4");
                self.refresh_probe();
            }
            if egui_glass::GlassButton::new(t!("transcode.from_gallery"))
                .show(ui)
                .clicked()
            {
                self.page = crate::app::Page::Gallery;
            }
        });
        ui.add_space(10.0);
        self.content_label(ui, t!("transcode.target"));
        // 与解码页 tab 同式：`GlassToolbar` 胶囊当条 + 8 个 `GlassButton` 连体横滚
        // （`spacing(2.0)`，不断尾；`id_salt` 隔离解码页 tab 的 ScrollArea ID）。
        egui::ScrollArea::horizontal()
            .id_salt("transcode_targets")
            .show(ui, |ui| {
                egui_glass::GlassToolbar::new(self.bar_style())
                    .spacing(2.0)
                    .show(ui, |ui| {
                        for v in TranscodeTarget::ALL {
                            if self.option_button(
                                ui,
                                self.transcode.target == v,
                                &crate::text::transcode_target_name(v),
                            ) {
                                self.transcode.target = v;
                            }
                        }
                    });
            });
        self.content_small(
            ui,
            crate::text::transcode_target_desc(self.transcode.target),
        );
        ui.add_space(10.0);
        self.content_label(ui, t!("transcode.res"));
        // 用户拍板：分辨率用文本框直输（`1920x1080`，空串=跟源），不用玻璃档位件；
        // 件库无文本件，此处用原语 TextEdit（AGENTS 件库约束为例外，用户明确要求）。
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.transcode.res_text)
                    .desired_width(140.0)
                    .hint_text("1920x1080"),
            );
            self.content_small(ui, t!("transcode.res_hint"));
        });
        if self.transcode.target == TranscodeTarget::H263 {
            self.content_small(ui, t!("transcode.res_h263"));
        }
        if self.transcode.target == TranscodeTarget::Raw {
            self.content_small(ui, t!("transcode.res_raw"));
        }
        ui.add_space(4.0);
        self.content_label(ui, t!("transcode.fps"));
        // 用户拍板：帧率用滑条，0 = 跟源，1-240 固定帧率（与壁纸 fps 滑条同构）。
        let fps_label = if self.transcode.fps_f.round() < 0.5 {
            t!("transcode.f_source").into_owned()
        } else {
            format!("{}fps", self.transcode.fps_f.round() as u32)
        };
        Self::content_slider(
            ui,
            self.dark,
            &mut self.transcode.fps_f,
            0.0..=240.0,
            fps_label,
            Some(1.0),
        );
        self.transcode.fps_f = self.transcode.fps_f.round().clamp(0.0, 240.0);
        self.content_small(ui, t!("transcode.fps_hint"));
        ui.add_space(4.0);
        self.content_label(ui, t!("transcode.seg"));
        // 用户拍板：分段用文本框直输帧数（默认 10）。
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.transcode.seg_text)
                    .desired_width(80.0)
                    .hint_text("10"),
            );
            self.content_small(ui, t!("transcode.seg_hint"));
        });
        // 硬解选卡（Win/Linux Vulkan；mac 无 Vulkan，list 恒空整行隐藏）。
        // 件库约束：单选开关全是 `GlassButton`（`option_button` 选中态描蓝 + ●）。
        if self.transcode.vk_adapters.is_none() {
            self.transcode.vk_adapters = Some(crate::vulkan_gpu::list_adapters());
        }
        let vk_adapters = self.transcode.vk_adapters.clone().unwrap_or_default();
        if !vk_adapters.is_empty() {
            use crate::vulkan_gpu::VulkanGpuPref;
            self.content_label(ui, t!("transcode.vk_gpu"));
            ui.horizontal_wrapped(|ui| {
                for (pref, name) in [
                    (VulkanGpuPref::Auto, t!("transcode.vk_auto")),
                    (VulkanGpuPref::Discrete, t!("transcode.vk_discrete")),
                    (VulkanGpuPref::Integrated, t!("transcode.vk_integrated")),
                ] {
                    if self.option_button(ui, self.transcode.vk_gpu == pref, &name) {
                        self.transcode.vk_gpu = pref.clone();
                        crate::vulkan_gpu::set_vulkan_gpu_pref(pref);
                    }
                }
                for a in &vk_adapters {
                    let pref = VulkanGpuPref::Named(a.name.clone());
                    if self.option_button(ui, self.transcode.vk_gpu == pref, &a.name) {
                        self.transcode.vk_gpu = pref.clone();
                        crate::vulkan_gpu::set_vulkan_gpu_pref(pref);
                    }
                }
            });
            if let Some(cur) = crate::codecs::current_vulkan_device_name() {
                self.content_small(ui, format!("{}: {cur}", t!("transcode.vk_current")));
            }
            ui.add_space(10.0);
        }
        ui.add_space(10.0);
        if !self.transcode.info.is_empty() {
            ui.horizontal(|ui| {
                self.content_label(ui, t!("transcode.probe"));
                self.content_monospace(ui, self.transcode.info.as_str());
            });
            ui.add_space(4.0);
        }
        let out_rel = out_path_for(self.transcode.target, &self.transcode.out_stem);
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(&out_rel)
            .display()
            .to_string();
        ui.horizontal(|ui| {
            self.content_label(ui, t!("transcode.output"));
            self.content_monospace(ui, out_full.as_str());
        });
        // 用户拍板：重命名用文本框直输 stem（空=默认名），不用系统弹窗。
        ui.horizontal(|ui| {
            self.content_label(ui, t!("transcode.rename"));
            ui.add(
                egui::TextEdit::singleline(&mut self.transcode.out_stem)
                    .desired_width(180.0)
                    .hint_text("video"),
            );
        });
        self.content_small(ui, t!("transcode.rename_hint"));
        // 默认名是活的时间戳：重命名空着时每秒重算显示
        // （落盘以点开始转码时刻为准）。
        if self.transcode.out_stem.trim().is_empty() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = if self.transcode.running() {
                t!("transcode.cancel")
            } else {
                t!("transcode.encode")
            };
            if egui_glass::GlassButton::new(label).show(ui).clicked() {
                if self.transcode.running() {
                    self.cancel_transcode_job();
                } else {
                    self.start_transcode_job();
                }
            }
            if let Some((done, total)) = self.transcode.progress {
                // 玻璃库无进度件（egui_glass 0.1.5 只有 Glass/Button/Toolbar/Slider，
                // 本地 registry 源码确认过），纯展示条用原语 ProgressBar（非交互件，
                // 不在禁用之列；嵌在同一行内，不另起 Panel）。
                let frac = if total > 0 {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!("{done}/{total}"))
                        .desired_width(180.0),
                );
            }
        });
        if !self.transcode.status.is_empty() {
            ui.add_space(4.0);
            self.content_small(ui, self.transcode.status.as_str());
        }
    }

    /// worker 进度轮询 + 收尾（worker 线程落盘，UI 只报状态）。
    pub(crate) fn poll_transcode_job(&mut self) {
        use rust_i18n::t;
        use std::sync::mpsc::TryRecvError;
        let mut finished: Option<Result<JobOut, String>> = None;
        let mut progress = None;
        let mut dead = false;
        if let Some(job) = self.transcode.job.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(JobMsg::Progress { done, total }) => progress = Some((done, total)),
                    Ok(JobMsg::Stage(s)) => {
                        self.transcode.status = match s {
                            JobStage::Decode => t!("transcode.stage_decode").into_owned(),
                            JobStage::Encode => t!("transcode.stage_encode").into_owned(),
                            JobStage::Mux => t!("transcode.stage_mux").into_owned(),
                        };
                    }
                    Ok(JobMsg::Finished(r)) => {
                        finished = Some(r);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    // 发送端没了但没发 Finished = worker 线程 panic 或提前退出，
                    // 不能停在“取消/解码中”装活。
                    Err(TryRecvError::Disconnected) => {
                        dead = true;
                        break;
                    }
                }
            }
        }
        if let Some(p) = progress {
            self.transcode.progress = Some(p);
        }
        if dead && finished.is_none() {
            finished = Some(Err(String::from(
                "transcode worker gone without finish (likely panicked; see log)",
            )));
        }
        if let Some(r) = finished {
            // 线程已发 Finished 即将退出，join 不阻塞；句柄收走免 detached 堆积。
            if let Some(job) = self.transcode.job.as_mut() {
                if let Some(h) = job.handle.take() {
                    let _ = h.join();
                }
            }
            self.transcode.job = None;
            self.transcode.progress = None;
            match r {
                Ok(out) => {
                    self.transcode.status = format!("done: {} ({} bytes)", out.path, out.bytes);
                    if out.hw_fallback {
                        // 回退提示双写：页内状态追加 + App 级 status 触发右上 toast。
                        self.transcode.status.push_str(" [CPU]");
                        self.status = t!("transcode.fallback_cpu").into_owned();
                    }
                }
                Err(e) if e == "cancelled" => {
                    // 用户主动取消：中性提示，不报 error。
                    self.transcode.status = t!("transcode.cancelled").into_owned();
                }
                Err(e) => {
                    self.transcode.status = format!("error: {e}");
                }
            }
        }
    }

    pub(crate) fn cancel_transcode_job(&mut self) {
        if let Some(job) = self.transcode.job.as_ref() {
            job.cancel.store(true, Ordering::Relaxed);
            // 立即清理界面状态（进度条清零）：worker 在下一个小批次边界看到旗子即退出，
            // poll 收尸时再写回最终状态；UI 不等 worker。
            self.transcode.progress = None;
            self.transcode.status = String::from("cancelling...");
        }
    }

    pub(crate) fn start_transcode_job(&mut self) {
        use rust_i18n::t;
        if self.transcode.source.is_empty() {
            self.transcode.status = t!("transcode.need_source").into_owned();
            return;
        }
        if !source_accepted(&self.transcode.source) {
            self.transcode.status = format!(
                "unsupported source (want .mp4/.png/.jpg/.bmp/.raw): {}",
                self.transcode.source
            );
            return;
        }
        // 先探片源（同步，快：只扫容器头 + 样片表）：行内直接报具体规格。
        match probe_source(&self.transcode.source) {
            Ok(info) => self.transcode.info = info.summary(),
            Err(e) => {
                self.transcode.status = format!("probe: {e}");
                self.transcode.info.clear();
                return;
            }
        }
        let res_wh = match parse_res_wh(&self.transcode.res_text) {
            Ok(wh) => wh,
            Err(e) => {
                self.transcode.status = format!("resolution: {e}");
                return;
            }
        };
        // 目标格式几何门（mp4 源 4K 直通跟源也可能超规，显式档同理）：
        // mpeg2v 只到 MP@HL（1920x1152），超了直接拒单，不断 job。
        if self.transcode.target == TranscodeTarget::Mpeg2v {
            if let Some((w, h)) = res_wh {
                if w > 1920 || h > 1152 {
                    self.transcode.status = String::from(
                        "resolution: mpeg2v max 1920x1152 (MP@HL), pick 1080p or lower",
                    );
                    return;
                }
            }
        }
        let fps_pos = self.transcode.fps_f.round().clamp(0.0, 240.0) as u32;
        let out_fps = if fps_pos == 0 { None } else { Some(fps_pos) };
        let seg = match parse_seg(&self.transcode.seg_text) {
            Ok(v) => v,
            Err(e) => {
                self.transcode.status = format!("segment: {e}");
                return;
            }
        };
        let source = self.transcode.source.clone();
        let out_rel = out_path_for(self.transcode.target, &self.transcode.out_stem).to_string();
        let out_full = std::path::Path::new(&crate::gallery::gallery_dir(&self.path))
            .join(&out_rel)
            .display()
            .to_string();
        let target = self.transcode.target;
        let (tx, rx): (Sender<JobMsg>, Receiver<JobMsg>) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = cancel.clone();
        let tx2 = tx.clone();
        let tx3 = tx.clone();
        let handle = std::thread::spawn(move || {
            let stage = move |s: JobStage| {
                let _ = tx2.send(JobMsg::Stage(s));
            };
            let progress = move |d: usize, t: usize| {
                let _ = tx3.send(JobMsg::Progress { done: d, total: t });
            };
            // panic 也必须落袋为 Finished(Err)，否则 UI 永远停在“解码中”。
            let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_transcode_job(
                    &source, target, &out_full, &cancel_w, stage, progress, res_wh, out_fps, seg,
                )
            })) {
                Ok(inner) => inner,
                Err(_) => Err(String::from("transcode worker panicked")),
            };
            let _ = tx.send(JobMsg::Finished(r));
        });
        self.transcode.job = Some(TranscodeJob {
            rx,
            cancel,
            handle: Some(handle),
        });
        self.transcode.progress = Some((0, 0));
        self.transcode.status = t!("transcode.running").into_owned();
    }
}
