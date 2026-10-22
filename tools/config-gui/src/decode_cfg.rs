//! `[decode.<fmt>]` 每格式解码表（从 `schema` 拆出：该文件已到 1000 行上限，
//! 解码子分类独立成模块；与固件 `src/config/decode.rs`（原子存储）+ `types.rs`
//! （`DecodeTable`/`DecodeEntry` schema）同构，改动三边同步）。
//!
//! RAW-GEOM：`width`/`height` 仅 `raw` 生效（小分辨率 raw 直搬；缺键 1920x1080
//! 照旧）；其余七格式残留键照读忽略。GUI 无 widgets（隐藏键），只做 schema
//! 透传 + `rules::clamp_decode` 钳制 + 黑盒 roundtrip。

use serde::{Deserialize, Serialize};

use crate::schema::{DecodeThreads, FeedMode, MediaFormat};

pub(crate) fn default_decode_buffers() -> u32 {
    0
}

// RAW-GEOM：与固件 `types::default_raw_width/height` 同值（缺键照旧）。
pub(crate) fn default_raw_width() -> u32 {
    1920
}

// RAW-GEOM：与固件 `types::default_raw_height` 同值（缺键照旧）。
pub(crate) fn default_raw_height() -> u32 {
    1080
}

/// 单格式解码开关（与固件 `[decode.<fmt>]` 同构；`buffers = 0` 跟随
/// `[render.tune] window`（1-32），显式 1-32 按格式覆盖；`tune.window` 保持全局默认）。
/// `fetch` 是逐格式供片覆盖（`None` = 继承全局 `[play] feed`，落盘不写键；
/// `Some` = 显式覆盖五档任一，见 `FeedMode`）。
/// `width`/`height` 是 RAW-GEOM（仅 `raw` 生效；其余格式残留键照读忽略，无 widgets）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DecodeConfig {
    pub(crate) threads: DecodeThreads,
    pub(crate) zero_copy: bool,
    pub(crate) streaming: bool,
    #[serde(default = "default_decode_buffers")]
    pub(crate) buffers: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fetch: Option<FeedMode>,
    // RAW-GEOM：raw 帧几何（其余格式忽略）。
    #[serde(default = "default_raw_width")]
    pub(crate) width: u32,
    #[serde(default = "default_raw_height")]
    pub(crate) height: u32,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self {
            threads: DecodeThreads::BspOnly,
            zero_copy: false,
            streaming: false,
            buffers: 0,
            fetch: None,
            // RAW-GEOM：缺表缺键回 1920x1080，老文件逐字节一致（落盘显式写出）。
            width: 1920,
            height: 1080,
        }
    }
}

/// `[decode]`：8 格式逐格式开关（与固件 `[decode.<fmt>]` 同构；
/// `qois`/`raw` 同表（前者帧级并行可兑现，后者零解码占位），缺表/缺键老文件全按默认照读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct DecodeTable {
    pub(crate) qois: DecodeConfig,
    pub(crate) raw: DecodeConfig,
    pub(crate) hevc: DecodeConfig,
    pub(crate) avc: DecodeConfig,
    pub(crate) vp9: DecodeConfig,
    pub(crate) h263: DecodeConfig,
    pub(crate) mpeg2v: DecodeConfig,
    pub(crate) theora: DecodeConfig,
}

impl DecodeTable {
    /// 解码页顺序（与 SPEC `gui_controls` 同序；`MediaFormat::ALL` 全 8 格式）。
    pub(crate) const ORDER: [MediaFormat; 8] = MediaFormat::ALL;
    pub(crate) fn get(&self, fmt: MediaFormat) -> Option<&DecodeConfig> {
        match fmt {
            MediaFormat::Qois => Some(&self.qois),
            MediaFormat::Raw => Some(&self.raw),
            MediaFormat::Hevc => Some(&self.hevc),
            MediaFormat::Avc => Some(&self.avc),
            MediaFormat::Vp9 => Some(&self.vp9),
            MediaFormat::H263 => Some(&self.h263),
            MediaFormat::Mpeg2v => Some(&self.mpeg2v),
            MediaFormat::Theora => Some(&self.theora),
        }
    }
    pub(crate) fn get_mut(&mut self, fmt: MediaFormat) -> Option<&mut DecodeConfig> {
        match fmt {
            MediaFormat::Qois => Some(&mut self.qois),
            MediaFormat::Raw => Some(&mut self.raw),
            MediaFormat::Hevc => Some(&mut self.hevc),
            MediaFormat::Avc => Some(&mut self.avc),
            MediaFormat::Vp9 => Some(&mut self.vp9),
            MediaFormat::H263 => Some(&mut self.h263),
            MediaFormat::Mpeg2v => Some(&mut self.mpeg2v),
            MediaFormat::Theora => Some(&mut self.theora),
        }
    }
}
