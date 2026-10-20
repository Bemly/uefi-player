//! Plane representation of the frame buffer (`CurrFrame`).
//!
//! The spec treats the frame buffer as a 3-dimensional array `CurrFrame[ plane ][ y ][ x ]`,
//! but this implementation holds each plane as a 1-dimensional `Vec<u16>` (row-major).
//!
//! `u16` regardless of `BitDepth`: 8-bit samples (0..=255) are stored widened rather than
//! packed, so every plane operation (get/set/loop filter/prediction) has exactly one code
//! path across all 3 bit depths -- narrowing to `u8` happens only at the output boundary
//! (see `crop_u8`, used for the `Frame`/`PlaneData::U8` output when `BitDepth == 8`).
//!
//! The buffer is allocated not at the display size (`FrameWidth`/`FrameHeight`) but at
//! the size rounded up to the superblock boundary (`Sb64Cols*64`/`Sb64Rows*64`, chroma
//! planes after subsampling). This is because the block boundary handling of spec
//! §6.4.21 `residual()` can cause blocks at the frame edge to write slightly beyond
//! `(MiCols*8, MiRows*8)` (the size just before the final display crop) (reads are
//! always clipped via `Min(maxX, ...)`, but writes to `pred[i][j]`/`Dequant[i][j]` are not clipped).

/// A single plane's buffer. Samples are stored as `u16` for every `BitDepth` (8/10/12-bit);
/// see the module doc for why.
///
/// A plane normally covers the whole frame (`x0 == 0`). A tile-parallel worker instead holds a
/// *column strip* ([`Plane::new_strip`]): a buffer covering only the absolute pixel columns
/// `[x0, x0 + width)`. All accessors keep taking **absolute** frame x coordinates -- the strip
/// origin is subtracted internally -- so the decode path is identical either way.
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{vec, vec::Vec};

#[derive(Debug, Clone)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    /// Absolute pixel column of this buffer's first column (0 for a whole-frame plane).
    pub x0: usize,
    data: Vec<u16>,
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Self {
        Self::new_strip(width, height, 0)
    }

    /// A column strip covering absolute pixel columns `[x0, x0 + width)` of a conceptual
    /// larger plane (used by the tile-parallel worker decoders, `tile::spawn_column_worker`).
    /// Accessors take absolute x; `x` must stay within the strip. The whole-frame `crop*`
    /// outputs are not meaningful on a strip.
    pub fn new_strip(width: usize, height: usize, x0: usize) -> Self {
        Self {
            width,
            height,
            x0,
            data: vec![0u16; width * height],
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u16 {
        debug_assert!(
            x >= self.x0 && x - self.x0 < self.width && y < self.height,
            "Plane::get out of range"
        );
        self.data[y * self.width + (x - self.x0)]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        debug_assert!(
            x >= self.x0 && x - self.x0 < self.width && y < self.height,
            "Plane::set out of range"
        );
        self.data[y * self.width + (x - self.x0)] = v;
    }

    /// Raw row-major buffer (stride == `width`), for the AVX2 SIMD convolution
    /// (`src/simd/inter.rs`), which proves its own bounds instead of paying `get`'s per-access
    /// check.
    #[inline]
    pub fn as_slice(&self) -> &[u16] {
        &self.data
    }

    /// Mutable counterpart of [`Plane::as_slice`], for the AVX2 loop-filter kernel
    /// (`src/simd/loop_filter.rs`), which writes its filtered samples directly into the raw
    /// buffer.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u16] {
        &mut self.data
    }

    /// Returns a row-major sample sequence cropped to the display size `(crop_width, crop_height)`.
    pub fn crop(&self, crop_width: usize, crop_height: usize) -> Vec<u16> {
        debug_assert_eq!(self.x0, 0, "crop is only meaningful on a whole-frame plane");
        let mut out = Vec::with_capacity(crop_width * crop_height);
        for y in 0..crop_height {
            let row_start = y * self.width;
            out.extend_from_slice(&self.data[row_start..row_start + crop_width]);
        }
        out
    }

    /// Same as [`Plane::crop`], narrowed to `u8` (for `PlaneData::U8` output when
    /// `BitDepth == 8`, where every sample is already known to fit in `0..=255`).
    pub fn crop_u8(&self, crop_width: usize, crop_height: usize) -> Vec<u8> {
        debug_assert_eq!(self.x0, 0, "crop is only meaningful on a whole-frame plane");
        let mut out = Vec::with_capacity(crop_width * crop_height);
        for y in 0..crop_height {
            let row_start = y * self.width;
            out.extend(
                self.data[row_start..row_start + crop_width]
                    .iter()
                    .map(|&v| v as u8),
            );
        }
        out
    }

    /// Same as [`Plane::crop`] but returns a new [`Plane`] instead of a `Vec<u16>`
    /// (for storing into `FrameStore` / DPB reference frame data per spec §8.10).
    pub fn crop_to_plane(&self, crop_width: usize, crop_height: usize) -> Plane {
        Plane {
            width: crop_width,
            height: crop_height,
            x0: 0,
            data: self.crop(crop_width, crop_height),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/framebuffer.rs"]
mod tests;
