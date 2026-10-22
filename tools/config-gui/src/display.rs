//! `[display]` 显示适配配置：与固件 `src/config/display.rs` 同构（fit/align 枚举/`file_str`/默认值 1:1，bar 为 RGB 自由色，改动双边同步）。
//!
//! 新文件原因：`schema.rs` 已到 1000 行上限，不再往里堆（见根 `AGENTS.md` 单文件 ≤1000 行）。
//! 纯数据类型 + 默认值，无 display 依赖；规则见 `rules`，文案见 `text`。
//!
//! TOML（`[display]` 表全可选，缺表缺键全默认，v3 bar 自由 RGB）：
//! ```toml
//! [display]
//! fit = "fit"       # stretch fit cover scale_down native，默认 fit
//! align = "center"  # center left right top bottom，默认 center
//! bar = "#102040"   # "black" | "white" | "gray" | "navy" | "#RRGGBB"（hex 大小写不敏感，
//!                   # 可带/不带前导 '#'，如 "#FF00AB"/"ff00ab"；默认 "black"）
//! ```
//! 未知字符串单键回默认（与固件同口径；固件侧 `log::warn!` 打串口，
//! GUI 侧无 `log` 轮子（禁新依赖）故静默回落，读盘状态行照常显示已载入，不断解析）。
//! 旧文件命名 bar（black/white/gray/navy）照读，新文件落盘一律小写无 '#' hex（如 "000000"）。

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 画面适配模式（默认 `Fit`：等比居中，黑边填充；与固件 `DisplayFit` 同构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DisplayFit {
    Stretch,
    #[default]
    Fit,
    Cover,
    ScaleDown,
    Native,
}

impl DisplayFit {
    /// 全集（声明序；`ALL` 下标即滑杆档位 0-4，与固件同序）。
    pub(crate) const ALL: [Self; 5] = [
        Self::Stretch,
        Self::Fit,
        Self::Cover,
        Self::ScaleDown,
        Self::Native,
    ];

    /// 配置文件拼写（落盘串；串口 `display:` 行同形，全小写，与固件 1:1）。
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Stretch => "stretch",
            Self::Fit => "fit",
            Self::Cover => "cover",
            Self::ScaleDown => "scale_down",
            Self::Native => "native",
        }
    }

    /// 滑杆档位 0-4（`ALL` 的下标，`Stretch=0`；未知回默认 `Fit=1`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(1)
    }

    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Stretch => 0,
            Self::Fit => 1,
            Self::Cover => 2,
            Self::ScaleDown => 3,
            Self::Native => 4,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Stretch,
            2 => Self::Cover,
            3 => Self::ScaleDown,
            4 => Self::Native,
            _ => Self::Fit,
        }
    }
}

impl Serialize for DisplayFit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.file_str())
    }
}

struct DisplayFitVisitor;

impl Visitor<'_> for DisplayFitVisitor {
    type Value = DisplayFit;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("one of \"stretch\", \"fit\", \"cover\", \"scale_down\", \"native\"")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<DisplayFit, E> {
        match v {
            "stretch" => Ok(DisplayFit::Stretch),
            "fit" => Ok(DisplayFit::Fit),
            "cover" => Ok(DisplayFit::Cover),
            "scale_down" => Ok(DisplayFit::ScaleDown),
            "native" => Ok(DisplayFit::Native),
            // GUI 无 `log` 轮子（禁新依赖），静默回默认；固件侧同值会 `log::warn!`。
            _ => Ok(DisplayFit::Fit),
        }
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<DisplayFit, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for DisplayFit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(DisplayFitVisitor)
    }
}

/// 黑边填充色（v3 自由 RGB；默认 `BLACK`；与固件 `BarColor` 同构 1:1）。
///
/// - 预设 4 色沿用 v1 命名（`black/white/gray/navy`），落盘后统一为 hex，读盘两形都认；
/// - `from_str` 命名 4 + hex 解析（大小写不敏感，可带/不带 '#'），失败回 `None`
///  （调用方 Deserialize 钳回 `BLACK`，固件侧同时 `log::warn!`）；
/// - `to_hex` 小写 `"rrggbb"`（无 '#'），落盘串 + 串口 `bar=<hex>` 同形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BarColor {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
}

impl Default for BarColor {
    fn default() -> Self {
        Self::BLACK
    }
}

impl BarColor {
    /// 预设 4 色（v1 命名行沿用；`black=(0,0,0)` `white=(255,255,255)`
    /// `gray=(128,128,128)` `navy=(0,0,128)`，与固件同值）。
    pub(crate) const BLACK: Self = Self { r: 0, g: 0, b: 0 };
    pub(crate) const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
    };
    pub(crate) const GRAY: Self = Self {
        r: 128,
        g: 128,
        b: 128,
    };
    pub(crate) const NAVY: Self = Self { r: 0, g: 0, b: 128 };

    /// 预设行（v1 4-way 即此行快设；声明序 black/white/gray/navy）。
    pub(crate) const PRESETS: [Self; 4] = [Self::BLACK, Self::WHITE, Self::GRAY, Self::NAVY];

    /// 预设名（按钮文案沿用 v1 落盘串；自定义色回 `None`，调用方用 hex 展示）。
    pub(crate) fn preset_name(self) -> Option<&'static str> {
        if self == Self::BLACK {
            Some("black")
        } else if self == Self::WHITE {
            Some("white")
        } else if self == Self::GRAY {
            Some("gray")
        } else if self == Self::NAVY {
            Some("navy")
        } else {
            None
        }
    }

    /// 解析命名 4 + hex（大小写不敏感，可带/不带 '#'；首尾空白忽略）。
    /// 失败回 `None`（Deserialize 钳回 `BLACK`；固件侧同值会 `log::warn!`）。
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "black" => return Some(Self::BLACK),
            "white" => return Some(Self::WHITE),
            "gray" => return Some(Self::GRAY),
            "navy" => return Some(Self::NAVY),
            _ => {}
        }
        let h = t.strip_prefix('#').unwrap_or(t);
        if h.len() != 6 {
            return None;
        }
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// 小写 `"rrggbb"`（无 '#'；落盘串 + 串口 `bar=<hex>` 同形，与固件 1:1）。
    pub(crate) fn to_hex(self) -> String {
        format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for BarColor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

struct BarColorVisitor;

impl Visitor<'_> for BarColorVisitor {
    type Value = BarColor;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("one of \"black\", \"white\", \"gray\", \"navy\", \"#RRGGBB\"")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<BarColor, E> {
        // GUI 无 `log` 轮子（禁新依赖），静默回默认；固件侧同值会 `log::warn!`。
        Ok(BarColor::from_str(v).unwrap_or(BarColor::BLACK))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<BarColor, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for BarColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(BarColorVisitor)
    }
}

/// 画面对齐方式（默认 Center 居中；与固件 DisplayAlign 同构）。
/// fit 与 native 下锚定画面位置，cover 下锚定裁切位置；stretch 忽略对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DisplayAlign {
    #[default]
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DisplayAlign {
    /// 全集（声明序；ALL 下标即按钮档位 0-4，与固件同序）。
    pub(crate) const ALL: [Self; 5] = [
        Self::Center,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];

    /// 配置文件拼写（落盘串；串口 display 行同形，全小写，与固件 1:1）。
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    /// 按钮档位 0-4（ALL 的下标，Center=0；未知回默认 Center=0）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }

    pub(crate) fn as_u8(self) -> u8 {
        match self {
            Self::Center => 0,
            Self::Left => 1,
            Self::Right => 2,
            Self::Top => 3,
            Self::Bottom => 4,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Top,
            4 => Self::Bottom,
            _ => Self::Center,
        }
    }
}

impl Serialize for DisplayAlign {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.file_str())
    }
}

struct DisplayAlignVisitor;

impl Visitor<'_> for DisplayAlignVisitor {
    type Value = DisplayAlign;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("one of \"center\", \"left\", \"right\", \"top\", \"bottom\"")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<DisplayAlign, E> {
        match v {
            "center" => Ok(DisplayAlign::Center),
            "left" => Ok(DisplayAlign::Left),
            "right" => Ok(DisplayAlign::Right),
            "top" => Ok(DisplayAlign::Top),
            "bottom" => Ok(DisplayAlign::Bottom),
            // GUI 无 log 轮子（禁新依赖），静默回默认；固件侧同值会打串口警告。
            _ => Ok(DisplayAlign::Center),
        }
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<DisplayAlign, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for DisplayAlign {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(DisplayAlignVisitor)
    }
}

/// `[display]`：画面适配 + 黑边色 + 对齐方式（缺表缺键全默认；未知字符串单键回默认，
/// 不触发整文件回默认，与固件同口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DisplayConfig {
    #[serde(default)]
    pub(crate) fit: DisplayFit,
    #[serde(default)]
    pub(crate) bar: BarColor,
    #[serde(default)]
    pub(crate) align: DisplayAlign,
}

#[cfg(test)]
mod display_blackbox {
    use super::*;
    use crate::rules::coerce_loaded;
    use crate::schema::FileConfig;

    /// 代表色采样：预设 4 + 自定义 3（`102040` 矩阵用色 + 纯红 + 近黑），
    /// fit 5 × align 5 × 7 色 = 175 组合不断字段。
    /// 主笛卡尔（`rules::blackbox_matrix::schema_roundtrip_matrix`）不加 display 维
    /// （×100 即 55 万组合），全量在此盖，不存在代表集缺口。
    const BAR_SAMPLES: [BarColor; 7] = [
        BarColor::BLACK,
        BarColor::WHITE,
        BarColor::GRAY,
        BarColor::NAVY,
        BarColor {
            r: 0x10,
            g: 0x20,
            b: 0x40,
        },
        BarColor {
            r: 255,
            g: 0,
            b: 171,
        },
        BarColor { r: 1, g: 2, b: 3 },
    ];

    #[test]
    fn display_roundtrip_matrix() {
        let mut n = 0;
        for f in DisplayFit::ALL {
            for b in BAR_SAMPLES {
                for a in DisplayAlign::ALL {
                    let mut cfg = FileConfig::default();
                    cfg.display.fit = f;
                    cfg.display.bar = b;
                    cfg.display.align = a;
                    let text = toml::to_string(&cfg).unwrap();
                    assert!(
                        text.contains(&format!("fit = \"{}\"", f.file_str())),
                        "{f:?} missing in {text}"
                    );
                    assert!(
                        text.contains(&format!("bar = \"{}\"", b.to_hex())),
                        "{b:?} missing in {text}"
                    );
                    assert!(
                        text.contains(&format!("align = \"{}\"", a.file_str())),
                        "{a:?} missing in {text}"
                    );
                    let back: FileConfig = toml::from_str(&text).unwrap();
                    assert_eq!(back.display.fit, f);
                    assert_eq!(back.display.bar, b);
                    assert_eq!(back.display.align, a);
                    assert_eq!(back, cfg);
                    // `fit/align` 的 `as_u8`/`from_u8`/`index` 往返（与固件同口径）；
                    // bar 无档位制，只验 `to_hex`/`from_str` 往返。
                    assert_eq!(DisplayFit::from_u8(f.as_u8()).file_str(), f.file_str());
                    assert_eq!(DisplayAlign::from_u8(a.as_u8()).file_str(), a.file_str());
                    assert_eq!(DisplayFit::ALL[f.index()], f);
                    assert_eq!(DisplayAlign::ALL[a.index()], a);
                    assert_eq!(BarColor::from_str(&b.to_hex()), Some(b));
                    n += 1;
                }
            }
        }
        assert_eq!(n, 5 * 7 * 5);
        // 预设 hex 口径（小写无 '#'，与固件 `to_hex` 1:1）。
        assert_eq!(BarColor::BLACK.to_hex(), "000000");
        assert_eq!(BarColor::WHITE.to_hex(), "ffffff");
        assert_eq!(BarColor::GRAY.to_hex(), "808080");
        assert_eq!(BarColor::NAVY.to_hex(), "000080");
        // 枚举顺序/默认值与固件 1:1（fit 默认 `Fit`；bar 默认 `BLACK`；align 默认 `Center`）。
        assert_eq!(
            DisplayFit::ALL,
            [
                DisplayFit::Stretch,
                DisplayFit::Fit,
                DisplayFit::Cover,
                DisplayFit::ScaleDown,
                DisplayFit::Native
            ]
        );
        assert_eq!(
            BarColor::PRESETS,
            [
                BarColor::BLACK,
                BarColor::WHITE,
                BarColor::GRAY,
                BarColor::NAVY
            ]
        );
        assert_eq!(FileConfig::default().display.fit, DisplayFit::Fit);
        assert_eq!(FileConfig::default().display.bar, BarColor::BLACK);
        assert_eq!(FileConfig::default().display.align, DisplayAlign::Center);
        assert_eq!(
            DisplayAlign::ALL,
            [
                DisplayAlign::Center,
                DisplayAlign::Left,
                DisplayAlign::Right,
                DisplayAlign::Top,
                DisplayAlign::Bottom
            ]
        );
    }

    /// hex 往返：大小写不敏感，可带/不带 '#'（落盘一律小写无 '#'）。
    #[test]
    fn display_bar_hex_roundtrip() {
        // 同一色四形全认。
        for s in ["#102040", "#102040", "102040", "102040"] {
            assert_eq!(
                BarColor::from_str(s),
                Some(BarColor {
                    r: 0x10,
                    g: 0x20,
                    b: 0x40
                }),
                "{s}"
            );
        }
        // 大小写混合。
        assert_eq!(BarColor::from_str("#FF00ab"), BarColor::from_str("ff00AB"),);
        assert_eq!(
            BarColor::from_str("#FF00AB"),
            Some(BarColor {
                r: 255,
                g: 0,
                b: 171
            }),
        );
        // 首尾空白忽略。
        assert_eq!(
            BarColor::from_str("  #102040  "),
            Some(BarColor {
                r: 0x10,
                g: 0x20,
                b: 0x40
            }),
        );
        // TOML 落盘 hex 再读回同色（大小写/#-less 全归一到小写无 '#'）。
        for s in ["#102040", "#102040", "#FF00AB", "ff00ab", "102040"] {
            let cfg: FileConfig =
                toml::from_str(&format!("version = 1\n[display]\nbar = \"{s}\"\n")).unwrap();
            let text = toml::to_string(&cfg).unwrap();
            assert!(
                text.contains(&format!("bar = \"{}\"", cfg.display.bar.to_hex())),
                "{s} -> {text}"
            );
            let back: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(back.display.bar, cfg.display.bar);
        }
    }

    /// 命名兼容 4（旧文件命名 bar 照读；大小写不敏感同样认）。
    #[test]
    fn display_bar_named_compat() {
        for (s, want) in [
            ("black", BarColor::BLACK),
            ("white", BarColor::WHITE),
            ("gray", BarColor::GRAY),
            ("navy", BarColor::NAVY),
            ("Black", BarColor::BLACK),
            ("WHITE", BarColor::WHITE),
        ] {
            assert_eq!(BarColor::from_str(s), Some(want), "{s}");
            let cfg: FileConfig =
                toml::from_str(&format!("version = 1\n[display]\nbar = \"{s}\"\n")).unwrap();
            assert_eq!(cfg.display.bar, want, "{s}");
        }
        // 预设名取回（按钮文案沿用 v1 落盘串；自定义色回 None 走 hex 展示）。
        assert_eq!(BarColor::BLACK.preset_name(), Some("black"));
        assert_eq!(BarColor::WHITE.preset_name(), Some("white"));
        assert_eq!(BarColor::GRAY.preset_name(), Some("gray"));
        assert_eq!(BarColor::NAVY.preset_name(), Some("navy"));
        assert_eq!(
            BarColor {
                r: 0x10,
                g: 0x20,
                b: 0x40
            }
            .preset_name(),
            None
        );
    }

    /// 缺表缺键全默认（`fit="fit"` + `bar=BLACK`，老文件照读）。
    #[test]
    fn display_missing_table_defaults() {
        // 空文件：全默认。
        let cfg: FileConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.display.fit, DisplayFit::Fit);
        assert_eq!(cfg.display.bar, BarColor::BLACK);
        assert_eq!(cfg.display.align, DisplayAlign::Center);
        // 远古文件（无 `[display]` 表）：照读，新字段全默认。
        let legacy: FileConfig =
            toml::from_str("version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n")
                .unwrap();
        assert_eq!(legacy.display.fit, DisplayFit::Fit);
        assert_eq!(legacy.display.bar, BarColor::BLACK);
        assert_eq!(legacy.display.align, DisplayAlign::Center);
        // 空表：全默认。
        let empty: FileConfig = toml::from_str("version = 1\n[display]\n").unwrap();
        assert_eq!(empty.display.fit, DisplayFit::Fit);
        assert_eq!(empty.display.bar, BarColor::BLACK);
        assert_eq!(empty.display.align, DisplayAlign::Center);
        // 缺键：只写一边，另一边默认。
        let only_fit: FileConfig =
            toml::from_str("version = 1\n[display]\nfit = \"native\"\n").unwrap();
        assert_eq!(only_fit.display.fit, DisplayFit::Native);
        assert_eq!(only_fit.display.bar, BarColor::BLACK);
        assert_eq!(only_fit.display.align, DisplayAlign::Center);
        // 老文件无 bar 键：默认 BLACK（v1/v2 无 bar 键文件同此）。
        let only_bar: FileConfig =
            toml::from_str("version = 1\n[display]\nbar = \"#102040\"\n").unwrap();
        assert_eq!(only_bar.display.fit, DisplayFit::Fit);
        assert_eq!(
            only_bar.display.bar,
            BarColor {
                r: 0x10,
                g: 0x20,
                b: 0x40
            }
        );
        assert_eq!(only_bar.display.align, DisplayAlign::Center);
        let only_align: FileConfig =
            toml::from_str("version = 1\n[display]\nalign = \"left\"\n").unwrap();
        assert_eq!(only_align.display.fit, DisplayFit::Fit);
        assert_eq!(only_align.display.bar, BarColor::BLACK);
        assert_eq!(only_align.display.align, DisplayAlign::Left);
        // v1 老文件（有 fit/bar 无 align）：新键回默认 center，旧值照读（含命名 bar）。
        let v1_nodisplay_align: FileConfig =
            toml::from_str("version = 1\n[display]\nfit = \"cover\"\nbar = \"white\"\n").unwrap();
        assert_eq!(v1_nodisplay_align.display.fit, DisplayFit::Cover);
        assert_eq!(v1_nodisplay_align.display.bar, BarColor::WHITE);
        assert_eq!(v1_nodisplay_align.display.align, DisplayAlign::Center);
    }

    /// 读盘钳制：未知字符串单键回默认（不断解析，与固件同口径）；
    /// `coerce_loaded` 不动 display（全档位合法，无钳制）。
    #[test]
    fn display_load_clamp() {
        // 未知值回默认（固件侧同值会 `log::warn!`，GUI 静默回落）。
        let unk: FileConfig = toml::from_str(
            "version = 1\n[display]\nfit = \"bogus\"\nbar = \"bogus\"\nalign = \"bogus\"\n",
        )
        .unwrap();
        assert_eq!(unk.display.fit, DisplayFit::Fit);
        assert_eq!(unk.display.bar, BarColor::BLACK);
        assert_eq!(unk.display.align, DisplayAlign::Center);
        // 垃圾 hex 全回 BLACK（长度/字符非法）。
        for s in [
            "", "#", "#12345", "#1234567", "#zzzzzz", "gggggg", "12345", "#10204g",
        ] {
            assert_eq!(BarColor::from_str(s), None, "{s}");
            let cfg: FileConfig =
                toml::from_str(&format!("version = 1\n[display]\nbar = \"{s}\"\n")).unwrap();
            assert_eq!(cfg.display.bar, BarColor::BLACK, "{s}");
        }
        // 单键未知：只回落该键。
        let half: FileConfig =
            toml::from_str("version = 1\n[display]\nfit = \"bogus\"\nbar = \"white\"\n").unwrap();
        assert_eq!(half.display.fit, DisplayFit::Fit);
        assert_eq!(half.display.bar, BarColor::WHITE);
        assert_eq!(half.display.align, DisplayAlign::Center);
        // 新模式未知回落只动该键：cover/scale_down/align 拼写必须精确。
        let cover: FileConfig =
            toml::from_str("version = 1\n[display]\nfit = \"cover\"\nalign = \"bogus\"\n").unwrap();
        assert_eq!(cover.display.fit, DisplayFit::Cover);
        assert_eq!(cover.display.align, DisplayAlign::Center);
        let scaled: FileConfig =
            toml::from_str("version = 1\n[display]\nfit = \"scale_down\"\nalign = \"top\"\n")
                .unwrap();
        assert_eq!(scaled.display.fit, DisplayFit::ScaleDown);
        assert_eq!(scaled.display.align, DisplayAlign::Top);
        // `from_u8` 未知回默认（与固件同口径；bar 无档位制，不参此）。
        assert_eq!(DisplayFit::from_u8(99), DisplayFit::Fit);
        assert_eq!(DisplayAlign::from_u8(1), DisplayAlign::Left);
        assert_eq!(DisplayAlign::from_u8(3), DisplayAlign::Top);
        assert_eq!(DisplayFit::from_u8(0), DisplayFit::Stretch);
        assert_eq!(DisplayFit::from_u8(2), DisplayFit::Cover);
        assert_eq!(DisplayFit::from_u8(3), DisplayFit::ScaleDown);
        assert_eq!(DisplayFit::from_u8(4), DisplayFit::Native);
        assert_eq!(DisplayAlign::from_u8(99), DisplayAlign::Center);
        assert_eq!(DisplayAlign::from_u8(0), DisplayAlign::Center);
        assert_eq!(DisplayAlign::from_u8(2), DisplayAlign::Right);
        assert_eq!(DisplayAlign::from_u8(4), DisplayAlign::Bottom);
        // `coerce_loaded`（读盘路径）不动 display。
        let mut cfg = FileConfig::default();
        cfg.display.fit = DisplayFit::Cover;
        cfg.display.bar = BarColor::GRAY;
        cfg.display.align = DisplayAlign::Right;
        let coerced = coerce_loaded(cfg);
        assert_eq!(coerced.display.fit, DisplayFit::Cover);
        assert_eq!(coerced.display.bar, BarColor::GRAY);
        assert_eq!(coerced.display.align, DisplayAlign::Right);
    }
}
