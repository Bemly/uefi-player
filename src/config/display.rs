//! `[display]` 显示适配配置（`src/config` 的 display 半；`types.rs` 已近 1000 行
//! 上限，故独立成文件，不再往里堆）。
//!
//! TOML（`[display]` 表全可选，缺表缺键全默认；v2 加 `cover`/`scale_down` 两档 fit
//! 与 5 档 `align`，旧文件无 `align` 键照读回默认 `center`，不断 boot）：
//! ```toml
//! [display]
//! fit = "fit"     # "stretch" | "fit" | "cover" | "scale_down" | "native"，默认 "fit"
//! align = "center" # "center" | "left" | "right" | "top" | "bottom"，默认 "center"
//! bar = "#102040" # 命名 4 色（旧文件照读）或自由 hex "#RRGGBB"，默认 "black"
//! ```
//! 未知字符串回默认 + 一行 `cfg:` 警告（`parse` 整文件回默认不断 boot，而是
//! 本表内单键回落，见枚举的 `Deserialize` 手写实现；与 `play.fps` 字符串三态
//! 的单键回落同口径，无别名）。
//! 与 `tools/config-gui` 侧同构（枚举顺序/`file_str`/默认值 1:1，改动双边同步）。
//!
//! 中英 word mapping（GUI 文案口径，见 `docs/config.md`）：拉伸=stretch，
//! 缩放最大=fit，填充=裁切=cover，缩放最小=scale_down（shrink-only），原生=native。

use core::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 画面适配模式（默认 `Fit`：等比居中，黑边填充）。
///
/// v2 档位顺序（`ALL` 下标即 GUI 滑杆档位）：Stretch=0，Fit=1（默认），Cover=2，
/// ScaleDown=3（shrink-only：放得下 1:1，否则按 fit 缩），Native=4（恒 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayFit {
    Stretch,
    #[default]
    Fit,
    Cover,
    ScaleDown,
    Native,
}

impl DisplayFit {
    /// 全集（声明序；`ALL` 下标即 GUI 滑杆档位 0-4）。
    pub const ALL: [Self; 5] = [
        Self::Stretch,
        Self::Fit,
        Self::Cover,
        Self::ScaleDown,
        Self::Native,
    ];

    /// 配置文件拼写（落盘串；串口 `display:` 行同形，全小写）。
    pub(crate) const fn file_str(self) -> &'static str {
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

    pub(crate) const fn as_u8(self) -> u8 {
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
            _ => {
                log::warn!(
                    "cfg: unknown display.fit \"{}\" -- using default \"fit\"",
                    v
                );
                Ok(DisplayFit::Fit)
            }
        }
    }

    fn visit_string<E: de::Error>(self, v: alloc::string::String) -> Result<DisplayFit, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for DisplayFit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(DisplayFitVisitor)
    }
}

/// 画面锚点（默认 `Center`；fit/scale_down 的黑边侧归属、cover/native-oversize 的
/// 裁切窗起点都按它锚定；stretch 忽略本键）。
///
/// 口径：`Left`/`Right` 只管横轴（放得下时贴左/右，放不下时裁左/裁右），
/// `Top`/`Bottom` 只管纵轴，不管的轴恒居中。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayAlign {
    #[default]
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DisplayAlign {
    /// 全集（声明序；`ALL` 下标即 GUI 滑杆档位 0-4）。
    pub const ALL: [Self; 5] = [
        Self::Center,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];

    /// 配置文件拼写（落盘串；串口 `display:` 行同形，全小写）。
    pub(crate) const fn file_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    /// 滑杆档位 0-4（`ALL` 的下标，`Center=0`；未知回默认 `Center=0`）。
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }

    pub(crate) const fn as_u8(self) -> u8 {
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
            _ => {
                log::warn!(
                    "cfg: unknown display.align \"{}\" -- using default \"center\"",
                    v
                );
                Ok(DisplayAlign::Center)
            }
        }
    }

    fn visit_string<E: de::Error>(self, v: alloc::string::String) -> Result<DisplayAlign, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for DisplayAlign {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(DisplayAlignVisitor)
    }
}

/// 黑边填充色（v3 自由 RGB，默认 `BLACK`；RGB 口径见 scaler 侧 `BltPixel::new(r,g,b)`）。
///
/// TOML 接受命名 4 色（全小写旧拼写，旧文件照读）+ 自由 hex `#RRGGBB`
/// （大小写无关，可带/不带前导 `#`，如 `#102040` / `102040` / `#FF0000`）；
/// 未知/垃圾字符串回默认 `BLACK` + 一行 `cfg:` 警告（与 fit/align 单键回落同口径）。
/// 与 `tools/config-gui` 侧同构（字段/常量/`from_str`/`to_hex`/默认值 1:1，
/// 改动双边同步；v1/v2 的命名 4 档 GUI 预设行即此 4 常量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl BarColor {
    /// 预设 4 色（v1/v2 命名档位，hex 口径：`000000`/`ffffff`/`808080`/`000080`）。
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
    };
    pub const GRAY: Self = Self {
        r: 128,
        g: 128,
        b: 128,
    };
    pub const NAVY: Self = Self { r: 0, g: 0, b: 128 };

    /// 解析命名 4 色 + hex（`#` 可选，大小写无关）；失败回 `None`
    /// （调用方按既有 coerce 口径回 `BLACK` + 警告，不另起新口径）。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "black" => Some(Self::BLACK),
            "white" => Some(Self::WHITE),
            "gray" => Some(Self::GRAY),
            "navy" => Some(Self::NAVY),
            _ => {
                let hex = s.strip_prefix('#').unwrap_or(s);
                if hex.len() != 6 {
                    return None;
                }
                let h = hex.as_bytes();
                let mut rgb = [0u8; 3];
                let mut i = 0;
                while i < 3 {
                    let hi = hex_val(h[i * 2])?;
                    let lo = hex_val(h[i * 2 + 1])?;
                    rgb[i] = (hi << 4) | lo;
                    i += 1;
                }
                Some(Self {
                    r: rgb[0],
                    g: rgb[1],
                    b: rgb[2],
                })
            }
        }
    }

    /// 小写 `rrggbb`（无 `#`；串口 `display:` 行 `bar=<hex>` 与落盘同形）。
    pub fn to_hex(self) -> alloc::string::String {
        alloc::format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// 单 hex 字符 → 半字节（大小写无关；非法回 `None`）。
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl Default for BarColor {
    fn default() -> Self {
        Self::BLACK
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
        match BarColor::from_str(v) {
            Some(c) => Ok(c),
            None => {
                log::warn!(
                    "cfg: unknown display.bar \"{}\" -- using default \"black\"",
                    v
                );
                Ok(BarColor::BLACK)
            }
        }
    }

    fn visit_string<E: de::Error>(self, v: alloc::string::String) -> Result<BarColor, E> {
        self.visit_str(&v)
    }
}

impl<'de> Deserialize<'de> for BarColor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(BarColorVisitor)
    }
}

/// `[display]`：画面适配 + 锚点 + 黑边色（缺表缺键全默认；未知字符串单键回默认 +
/// 警告，不触发整文件回默认；旧文件无 `align` 键按 `#[serde(default)]` 回 `Center`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    #[serde(default)]
    pub fit: DisplayFit,
    #[serde(default)]
    pub align: DisplayAlign,
    #[serde(default)]
    pub bar: BarColor,
}
