//! 液态玻璃主题色板（唯一出口）：前景字 / 强调色两档 / 轨道 / 占位 / 玻璃材质。
//!
//! 收敛前同一语义散落多套值（强调色 `LIGHT_BLUE` / `#0088FF` / `(0,90,200)` 三处各写，
//! 轨道 45/26 与 28/28 两套，玻璃预设 `bar_style`/`content_slider` 各判一遍），
//! 深浅切换时各件观感对不齐。现统一走这里，调色只改一处。
//!
//! - 字色恒纯白/纯黑（用户拍板：不用 visuals 默认灰），描边取反色。
//! - 强调色分两档：`accent_fill` 填充/描边/进度条用 Apple Liquid Glass 系统蓝
//!   （iOS 26 浅 `#0088FF` / 深 `#0091FF`，与 `egui_glass` 滑杆同源）；
//!   `accent_text` 是玻璃上的选中字色，同色相压暗（浅）/提亮（深）保对比度——
//!   旧深色 `LIGHT_BLUE`（173,216,230）饱和度太低，选中态与普通白字几乎分不出。
//! - 无新轮子：纯常量 + `egui_glass` 现有预设，无 GUI 件变更（件库锁死不受影响）。

use eframe::egui::Color32;

/// 正文/标题前景（纯白/纯黑）。
pub(crate) fn fg(dark: bool) -> Color32 {
    if dark {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// 描边罩底色（前景的反色）。
pub(crate) fn outline(dark: bool) -> Color32 {
    if dark {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// 强调填充：滑杆已填段、toast 进度、焦点环、选中卡描边。
pub(crate) fn accent_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0, 145, 255)
    } else {
        Color32::from_rgb(0, 136, 255)
    }
}

/// 强调字色：玻璃上的选中文字（同色相，按底色调明度保可读）。
pub(crate) fn accent_text(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(110, 185, 255)
    } else {
        Color32::from_rgb(0, 90, 200)
    }
}

/// 轨道底（滑杆槽、toast 进度槽同值）。
pub(crate) fn track(dark: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(45)
    } else {
        Color32::from_black_alpha(26)
    }
}

/// 无封面占位块底色（图库卡片）。
pub(crate) fn placeholder(dark: bool) -> Color32 {
    if dark {
        Color32::from_black_alpha(80)
    } else {
        Color32::from_white_alpha(120)
    }
}

/// 工具栏/toast/滑杆的玻璃材质：深色配深色预设，浅色配默认。
pub(crate) fn glass(dark: bool) -> egui_glass::GlassStyle {
    if dark {
        egui_glass::GlassStyle::dark()
    } else {
        egui_glass::GlassStyle::regular()
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;

    /// 相对亮度（sRGB 近似线性化，够判对比方向）。
    fn luma(c: Color32) -> f32 {
        let f = |v: u8| {
            let s = f32::from(v) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (luma(a), luma(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn fg_and_outline_are_inverse() {
        for d in [false, true] {
            assert_ne!(fg(d), outline(d));
            assert_eq!(fg(d), outline(!d));
        }
    }

    /// 选中字色必须与底色（深=黑罩/浅=白罩，即描边色）有 WCAG AA 大字 3:1 以上对比，
    /// 且明显区别于普通前景（防回到旧 LIGHT_BLUE 那种“看不出选中”）。
    #[test]
    fn accent_text_readable_and_distinct() {
        for d in [false, true] {
            assert!(
                contrast(accent_text(d), outline(d)) >= 3.0,
                "dark={d}: {:.2}",
                contrast(accent_text(d), outline(d))
            );
            let a = accent_text(d);
            let f = fg(d);
            let dist = (i32::from(a.r()) - i32::from(f.r())).abs()
                + (i32::from(a.g()) - i32::from(f.g())).abs()
                + (i32::from(a.b()) - i32::from(f.b())).abs();
            assert!(dist > 150, "dark={d}: accent too close to fg ({dist})");
        }
    }

    #[test]
    fn glass_preset_follows_theme() {
        assert!(glass(true).is_dark());
        assert!(!glass(false).is_dark());
    }
}
