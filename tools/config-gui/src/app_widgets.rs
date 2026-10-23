//! 中央透明区玻璃小件（同 `impl App`，`app.rs` 触达即拆：跨文件只为守 1000 行上限，行为零改）。
//!
//! - `show_toast`/`bar_style`/`accent`/`marker`：主题与 toast。
//! - `outlined_*`/`content_*`：中央描边字四件套（库内 `ui.label` 无描边可挂，手写罩底，见注释）。
//! - `content_slider`/`paint_slider_bar`：复刻 `GlassSlider` 布局的描边表头版滑杆
//!   （常数与库内同值，库 pin 死后同步跟；内含 `DragValue`/`Slider` 是玻璃 subterfuge，
//!   非新增普通控件，见函数头注释）。

use eframe::egui;

use super::{Toast, TOAST_SECS};
use crate::app::App;

impl App {
    /// 选中态强调色（玻璃上的选中字色，见 `theme::accent_text`；填充/描边用 `theme::accent_fill`）。
    pub(crate) fn accent(dark: bool) -> egui::Color32 {
        crate::theme::accent_text(dark)
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
        let accent = crate::theme::accent_fill(self.dark);
        let track = crate::theme::track(self.dark);
        egui::Area::new("toast".into())
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 16.0))
            .show(ctx, |ui| {
                egui_glass::Glass::new(self.bar_style())
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        // 纯白/纯黑字（用户拍板：不要主题灰）：暗色白字，浅色黑字。
                        let fg = crate::theme::fg(self.dark);
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
        crate::theme::glass(self.dark)
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
        let (fg, stroke) = (crate::theme::fg(dark), crate::theme::outline(dark));
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
        let style = crate::theme::glass(dark);
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
        let accent = crate::theme::accent_fill(dark);
        let track_color = crate::theme::track(dark);
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
}
