//! 图库页：与 `player.toml` 同目录的媒体浏览（N×M 网格卡片：上封面下简介）。
//!
//! 图库目录 = 已载入 `player.toml` 的父目录（默认 `"player.toml"` → `"."`），
//!   与配置同盘同目录，U盘上即插即看。固件认的布局是 `1080p/video.*`（播片）
//!   + `wallpaper/*`（壁纸），页内只列这两处的文件，不递归整盘。
//!
//! 轮子结论（见根 `AGENTS.md`「轮子优先」，不复述版本）：封面不引新轮子——
//! `qois` 首帧走已有 `qoi` 箱，`raw` 首帧直读 BGRA，`mp4` 首显示帧复用转码页
//! `demux_groups` + `decode_group`（只解前两组，到首帧即停），静帧经 `image`
//! 缩放；纹理走 egui 托管（UI 线程 `load_texture`，双 renderer 自动镜像，
//! 见 `egui_glass live.rs` 同款，无需 `register_native_texture`）。
//! `.hevc/.265/.ivf/.h263/.m2v/.ogv` 无封面（GUI 无裸流 demux/解码链：
//! h263/mpeg2v/theora 解码在固件 vendor 树，ivf 缺 IVF 解析轮子；
//! 且转码页本来就只产 `video.mp4` + 裸流，mp4 覆盖 avc/hevc/vp9 三轨），
//! 一律占位块 + 格式大字，不断任何流程。
//! 卡片只用库内件：`Glass::panel()` 容器（半径 28，与侧栏同语言）+ 图 +
//! 纯文本（无玻璃嵌玻璃，点击走 `ui.interact` 原语）；选中态描边 +
//! 标题 accent 色（Mirage 式蓝框对标无 ●/○ 字形）。
//!
//! 性能纪律：解码/缩放全在后台线程（首帧即发，逐张流式回传），UI 只做
//! `load_texture` + 缓存；封面宽封顶 320（单张约 230KB）；显示封顶与壁纸同档。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use crate::app::App;

/// 卡片宽（pt；封面 16:9，简介两行；列数由可用宽自适应）。
pub(crate) const CARD_W: f32 = 220.0;
/// 卡片间距（pt，行列同值）。
pub(crate) const CARD_GAP: f32 = 12.0;
/// 封面宽封顶（pt/px 等比；单张约 230KB，十张常驻约 2MB）。
pub(crate) const COVER_W: u32 = 320;

/// 图库一行（文件名 + 字节数 + 固件侧格式标签 + 封面元数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GalleryEntry {
    pub(crate) name: String,
    /// 相对图库根的显示路径（如 `1080p/video.mp4`）。
    pub(crate) size: u64,
    pub(crate) kind: &'static str,
    /// 探针编解码（`probe_source().codec`，封面线程回填；None = 还没回来/探不动）。
    /// 动态背景选片门只认 `avc`/`hevc`（见 `video_bg::resolve_source`）。
    pub(crate) codec: Option<&'static str>,
    /// 封面线程回填（None = 还没回来/不支持）。
    pub(crate) meta: Option<ThumbMeta>,
}

/// 封面元数据（几何 + 计时；计时仅 mp4 有）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThumbMeta {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) fps: u32,
    pub(crate) n: usize,
    pub(crate) dur_s: u64,
    /// 计时行是否展示（mp4 真值；静帧/qois/raw 无时间轴）。
    pub(crate) has_time: bool,
}

/// 封面线程 → UI 消息（逐张流式；Done 收尾）。
enum ThumbMsg {
    One {
        gen: u64,
        idx: usize,
        meta: ThumbMeta,
        codec: Option<&'static str>,
        cover: Option<(u32, u32, Vec<u8>)>,
    },
    Done {
        gen: u64,
    },
}

/// 自适应列数（纯规则，可单测）：可用宽能摆几张就几张，至少 1。
pub(crate) fn grid_columns(avail_w: f32) -> usize {
    (((avail_w + CARD_GAP) / (CARD_W + CARD_GAP)).floor() as usize).max(1)
}

/// 封面目标尺寸（纯规则，可单测）：宽封顶等比缩，至少 1px。
pub(crate) fn thumb_target(w: u32, h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (COVER_W, COVER_W * 9 / 16);
    }
    if w <= COVER_W {
        return (w, h);
    }
    let tw = COVER_W;
    let th = ((h as u64 * tw as u64).div_ceil(w as u64) as u32).max(1);
    (tw, th)
}

/// 字节数 → 人读串（纯规则，可单测）：B/KB/MB 一位小数。
pub(crate) fn human_size(n: u64) -> String {
    if n < 1024 {
        return format!("{n}B");
    }
    if n < 1024 * 1024 {
        let kb = n as f64 / 1024.0;
        return format!("{kb:.1}KB");
    }
    format!("{:.1}MB", n as f64 / (1024.0 * 1024.0))
}

/// 文件名截断（纯规则，可单测）：按字符截（CJK 安全），超了挂 …。
pub(crate) fn truncate_name(name: &str, max_chars: usize) -> String {
    let count = name.chars().count();
    if count <= max_chars {
        return name.to_owned();
    }
    let mut s: String = name.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// 简介行（纯规则，可单测）：第二行恒有，第三行仅计时媒体。
pub(crate) fn meta_lines(kind: &str, meta: &ThumbMeta, size: u64) -> (String, Option<String>) {
    let second = if meta.w > 0 && meta.h > 0 {
        format!("{} · {}×{} · {}", kind, meta.w, meta.h, human_size(size))
    } else {
        format!("{} · {}", kind, human_size(size))
    };
    let third = meta.has_time.then(|| {
        format!(
            "{}fps · {}s · {}帧",
            meta.fps.max(1),
            meta.dur_s,
            meta.n.max(1)
        )
    });
    (second, third)
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
                codec: None,
                meta: None,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 首帧封面（后台线程调用；只搬运+复用解码链，无新轮子）。
/// 返回下采样后 RGBA（宽 ≤320）+ 原始几何；不支持/失败即 None（占位）。
fn cover_first_frame(full: &str) -> Option<(u32, u32, Vec<u8>, u32, u32)> {
    let lower = full.to_ascii_lowercase();
    if lower.ends_with(".qois") {
        let data = std::fs::read(full).ok()?;
        if data.len() < 8 {
            return None;
        }
        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let frame = data.get(4..4 + len)?;
        let (head, px) = qoi::decode_to_vec(frame).ok()?;
        return downscale(head.width, head.height, &px);
    }
    if lower.ends_with(".raw") {
        // 固件壁纸帧：1920x1080 BGRA 直存，首帧即封面。
        let data = std::fs::read(full).ok()?;
        if data.len() < 1920 * 1080 * 4 {
            return None;
        }
        let mut px = data[..1920 * 1080 * 4].to_vec();
        let (chunks, _) = px.as_chunks_mut::<4>();
        for p in chunks {
            p.swap(0, 2);
        }
        return downscale(1920, 1080, &px);
    }
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
    {
        let img = image::open(full).ok()?.to_rgba8();
        let (w, h) = (img.width(), img.height());
        return downscale(w, h, &img.into_raw());
    }
    if lower.ends_with(".mp4") {
        return cover_mp4(full);
    }
    None
}

/// mp4 首显示帧（demux 首轨 + 前两组解码，到首帧即停；与转码页同链）。
fn cover_mp4(full: &str) -> Option<(u32, u32, Vec<u8>, u32, u32)> {
    use crate::schema::MediaFormat;
    use crate::transcode::{decode_group, demux_groups, TrackDecoder};
    let data = std::fs::read(full).ok()?;
    // 先探轨（容器探针，与转码页同顺序 hevc>avc>vp9 由 probe 定首轨）。
    let probe = crate::transcode::probe_source(full).ok()?;
    let want = match probe.codec {
        "avc" => MediaFormat::Avc,
        "hevc" => MediaFormat::Hevc,
        "vp9" => MediaFormat::Vp9,
        _ => return None,
    };
    let d = demux_groups(&data, want).ok()?;
    let mut dec = TrackDecoder::new(want);
    for g in d.groups.iter().take(2) {
        let frames = decode_group(&mut dec, &d, g).ok()?;
        if let Some(f) = frames.first() {
            let rgba = crate::codecs::yuv420_to_rgba(f);
            return downscale(rgba.w, rgba.h, &rgba.px);
        }
    }
    None
}

/// 下采样到封面尺寸（`image` Triangle，与转码页缩放同轮）。
/// 返回 (目标宽, 目标高, 小图px, 原宽, 原高)。
fn downscale(w: u32, h: u32, px: &[u8]) -> Option<(u32, u32, Vec<u8>, u32, u32)> {
    if w == 0 || h == 0 || px.len() < w as usize * h as usize * 4 {
        return None;
    }
    let (tw, th) = thumb_target(w, h);
    if (tw, th) == (w, h) {
        return Some((tw, th, px[..w as usize * h as usize * 4].to_vec(), w, h));
    }
    let img = image::RgbaImage::from_raw(w, h, px[..w as usize * h as usize * 4].to_vec())?;
    let small = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
    Some((tw, th, small.into_raw(), w, h))
}

/// 封面 worker（后台线程；逐张流式回传，失败即占位，不 halt）。
fn thumb_worker(jobs: Vec<String>, gen: u64, tx: Sender<ThumbMsg>) {
    for (idx, full) in jobs.into_iter().enumerate() {
        // 计时行复用探针（mp4/image/raw 有；裸流无则只剩几何行）。
        let probe = crate::transcode::probe_source(&full).ok();
        let codec = probe.as_ref().map(|p| p.codec);
        let cover = cover_first_frame(&full);
        let (gw, gh) = cover
            .as_ref()
            .map(|(w, h, ..)| (*w, *h))
            .or_else(|| probe.as_ref().map(|p| (p.w, p.h)))
            .unwrap_or((0, 0));
        let (fps, n, dur_s, has_time) = match probe {
            Some(p) => (p.fps, p.n, p.dur_s, p.container == "mp4"),
            None => (0, 0, 0, false),
        };
        let msg = ThumbMsg::One {
            gen,
            idx,
            meta: ThumbMeta {
                w: gw,
                h: gh,
                fps,
                n,
                dur_s,
                has_time,
            },
            codec,
            cover: cover.map(|(tw, th, px, ..)| (tw, th, px)),
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
    let _ = tx.send(ThumbMsg::Done { gen });
}

/// 图库状态（`App` 持有；`dir` 随 `player.toml` 路径走，换文件即换库）。
pub(crate) struct GalleryState {
    pub(crate) entries: Vec<GalleryEntry>,
    pub(crate) selected: Option<usize>,
    /// 封面纹理缓存（下标 → 托管纹理；UI 线程建，双 renderer 自动镜像）。
    covers: HashMap<usize, eframe::egui::TextureHandle>,
    pending: Option<Receiver<ThumbMsg>>,
    gen: u64,
}

impl GalleryState {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: None,
            covers: HashMap::new(),
            pending: None,
            gen: 0,
        }
    }

    pub(crate) fn refresh(&mut self, player_path: &str) {
        let dir = gallery_dir(player_path);
        self.entries = list_gallery(&dir);
        self.selected = None;
        self.covers.clear();
        self.pending = None;
        self.gen += 1;
        let gen = self.gen;
        let jobs: Vec<String> = self
            .entries
            .iter()
            .map(|e| {
                std::path::Path::new(&dir)
                    .join(&e.name)
                    .display()
                    .to_string()
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<ThumbMsg>();
        self.pending = Some(rx);
        std::thread::Builder::new()
            .name(String::from("gallery-thumbs"))
            .spawn(move || thumb_worker(jobs, gen, tx))
            .ok();
    }

    /// 收封面消息（UI 线程调用；纹理必须在 UI 线程建）。
    fn poll(&mut self, ctx: &eframe::egui::Context) {
        let mut done = false;
        if let Some(rx) = self.pending.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(ThumbMsg::One {
                        gen,
                        idx,
                        meta,
                        codec,
                        cover,
                    }) => {
                        if gen != self.gen {
                            continue;
                        }
                        if let Some(e) = self.entries.get_mut(idx) {
                            e.meta = Some(meta);
                            e.codec = codec;
                        }
                        if let Some((tw, th, px)) = cover {
                            if let Some(e) = self.entries.get(idx) {
                                let tex = ctx.load_texture(
                                    format!("gallery-cover-{gen}-{idx}-{}", e.name),
                                    eframe::egui::ColorImage::from_rgba_unmultiplied(
                                        [tw as usize, th as usize],
                                        &px,
                                    ),
                                    eframe::egui::TextureOptions::LINEAR,
                                );
                                self.covers.insert(idx, tex);
                            }
                        }
                    }
                    Ok(ThumbMsg::Done { gen }) => {
                        if gen == self.gen {
                            done = true;
                            break;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
        }
        if done {
            self.pending = None;
        } else if self.pending.is_some() {
            ctx.request_repaint();
        }
    }
}

impl App {
    /// 图库页：目录行 + 刷新/转码 + N×M 网格卡片（上封面下简介）。
    pub(crate) fn gallery_page(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        use rust_i18n::t;
        self.gallery.poll(ui.ctx());
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
            // 选中卡一键带源跳转转码页（源路径 = 图库根 + 相对名）。
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
        let cols = grid_columns(ui.available_width());
        let dark = self.dark;
        let n = self.gallery.entries.len();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut idx = 0;
            while idx < n {
                ui.horizontal(|ui| {
                    for _ in 0..cols {
                        if idx >= n {
                            break;
                        }
                        self.gallery_card(ui, idx, dark);
                        ui.add_space(CARD_GAP);
                        idx += 1;
                    }
                });
                ui.add_space(CARD_GAP);
            }
        });
    }

    /// 单张卡片：玻璃容器（封面顶满上沿）+ 标题 + 元数据两行（等高）；
    /// 整卡可点（`interact` 原语），选中描 accent 边。
    fn gallery_card(&mut self, ui: &mut eframe::egui::Ui, idx: usize, dark: bool) {
        use eframe::egui;
        let (name, kind, size, meta, sel) = match self.gallery.entries.get(idx) {
            Some(e) => (
                e.name.clone(),
                e.kind,
                e.size,
                e.meta,
                self.gallery.selected == Some(idx),
            ),
            None => return,
        };
        let tex = self.gallery.covers.get(&idx).cloned();
        let card = if dark {
            egui_glass::Glass::panel_dark()
        } else {
            egui_glass::Glass::panel()
        };
        // 封面贴上沿左右沿（与 panel 同半径 28上圆下直），文字区留边。
        let inner = card
            .inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 0,
                bottom: 10,
            })
            .show(ui, |ui| {
                // Glass 子 UI 会继承行横向布局此处强制竖排（否则封面与文字横排）。
                ui.vertical(|ui| {
                    ui.set_width(CARD_W);
                    // 封面（16:9 顶满；无纹理即占位块 + 格式大字，同圆角）。
                    let ch = CARD_W * 9.0 / 16.0;
                    let top_round = egui::CornerRadius {
                        nw: 28,
                        ne: 28,
                        sw: 0,
                        se: 0,
                    };
                    if let Some(t) = tex.as_ref() {
                        ui.add(
                            egui::Image::new(t)
                                .fit_to_exact_size(egui::vec2(CARD_W, ch))
                                .corner_radius(top_round)
                                .sense(egui::Sense::hover()),
                        );
                    } else {
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(CARD_W, ch), egui::Sense::hover());
                        ui.painter().rect_filled(
                            r,
                            top_round,
                            if dark {
                                egui::Color32::from_black_alpha(80)
                            } else {
                                egui::Color32::from_white_alpha(120)
                            },
                        );
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            kind.to_uppercase(),
                            egui::FontId::proportional(28.0),
                            // 纯白/纯黑（用户拍板：不用灰字，与其他字对齐）。
                            if dark {
                                egui::Color32::WHITE
                            } else {
                                egui::Color32::BLACK
                            },
                        );
                    }
                    ui.add_space(6.0);
                    // 文字区（左右各 8；标题 + 两行元数据恒三行等高，缺计时行垫空格）。
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.set_width(CARD_W - 16.0);
                            let title = truncate_name(&name, 26);
                            if sel {
                                ui.label(
                                    egui::RichText::new(title)
                                        .strong()
                                        .color(Self::accent(dark)),
                                );
                            } else {
                                // 纯白/纯黑（用户拍板：不用灰字/默认 visuals 灰）。
                                ui.label(egui::RichText::new(title).strong().color(if dark {
                                    egui::Color32::WHITE
                                } else {
                                    egui::Color32::BLACK
                                }));
                            }
                            let meta_v = meta.unwrap_or(ThumbMeta {
                                w: 0,
                                h: 0,
                                fps: 0,
                                n: 0,
                                dur_s: 0,
                                has_time: false,
                            });
                            let (second, third) = meta_lines(kind, &meta_v, size);
                            // 纯白/纯黑小字（用户拍板：去 `.weak()` 灰，与其他字对齐）。
                            let meta_fg = if dark {
                                egui::Color32::WHITE
                            } else {
                                egui::Color32::BLACK
                            };
                            ui.label(egui::RichText::new(second).small().color(meta_fg));
                            ui.label(
                                egui::RichText::new(third.unwrap_or_else(|| String::from(" ")))
                                    .small()
                                    .color(meta_fg),
                            );
                        });
                    });
                });
            });
        // 整卡点击（玻璃只给 hover sense这里用原语升级；卡内无玻璃件）。
        let click = ui.interact(
            inner.response.rect,
            egui::Id::new(("gallery-card", name.as_str())),
            egui::Sense::click(),
        );
        if click.clicked() {
            self.gallery.selected = Some(idx);
        }
        // 选中描边（Mirage 式蓝框；半径与 panel 一致）。
        if sel {
            ui.painter().rect_stroke(
                inner.response.rect,
                28.0,
                egui::Stroke::new(2.0, Self::accent(dark)),
                egui::StrokeKind::Outside,
            );
        }
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

    #[test]
    fn columns_adapt_to_width() {
        assert_eq!(grid_columns(100.0), 1);
        assert_eq!(grid_columns(CARD_W), 1);
        assert_eq!(grid_columns(CARD_W * 2.0 + CARD_GAP), 2);
        assert_eq!(grid_columns(1024.0), 4);
    }

    #[test]
    fn thumb_target_caps_width() {
        assert_eq!(thumb_target(1920, 1080), (320, 180));
        assert_eq!(thumb_target(160, 120), (160, 120));
        assert_eq!(thumb_target(0, 0), (COVER_W, COVER_W * 9 / 16));
        // 奇数高按比例向上取整，不断几何。
        assert_eq!(thumb_target(640, 359), (320, 180));
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(2048), "2.0KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0MB");
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate_name("abc", 5), "abc");
        assert_eq!(truncate_name("abcdef", 5), "abcd…");
        // 中文按字符截，不断字形。
        assert_eq!(truncate_name("图库测试文件名", 4), "图库测…");
    }

    #[test]
    fn meta_lines_shape() {
        let m = ThumbMeta {
            w: 1280,
            h: 720,
            fps: 60,
            n: 240,
            dur_s: 4,
            has_time: true,
        };
        let (s2, s3) = meta_lines("mp4", &m, 1024);
        assert_eq!(s2, "mp4 · 1280×720 · 1.0KB");
        assert_eq!(s3, Some(String::from("60fps · 4s · 240帧")));
        let m2 = ThumbMeta {
            has_time: false,
            ..m
        };
        let (s2b, s3b) = meta_lines("qois", &m2, 100);
        assert_eq!(s2b, "qois · 1280×720 · 100B");
        assert_eq!(s3b, None);
    }
}
