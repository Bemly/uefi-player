//! vp9dec: a from-scratch VP9 video decoder (zero dependency crates).
//!
//! Reference spec: VP9 Bitstream & Decoding Process Specification v0.7
//! (Google, dated 2017-02-22, <https://storage.googleapis.com/downloads.webmproject.org/docs/vp9/vp9-bitstream-specification-v0.7-20170222-draft.pdf>)
//!
//! Decodes all four VP9 profiles (8/10/12-bit; 4:2:0/4:2:2/4:4:0/4:4:4), verified bit-exact
//! against the full official conformance corpus. Runtime-detected AVX2 fast paths and
//! tile-parallel decoding accelerate the scalar reference path without altering its output.
//! See README.md "Current architecture" for the module map.

// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): always in scope; `use alloc::…` below resolves in both
// std and no_std builds.
extern crate alloc;

pub mod ivf;

// Every module below is internal; public only so the pure-std integration tests in
// tests/ can reach it -- not a stable API.
#[doc(hidden)]
pub mod bench_timing;
#[doc(hidden)]
pub mod bit_reader;
#[doc(hidden)]
pub mod bool_coder;
#[doc(hidden)]
pub mod common;
#[doc(hidden)]
pub mod compressed_header;
#[doc(hidden)]
pub mod counts;
#[doc(hidden)]
pub mod dpb;
#[doc(hidden)]
pub mod framebuffer;
#[doc(hidden)]
pub mod header;
#[doc(hidden)]
pub mod loop_filter;
#[doc(hidden)]
pub mod mv_ref_tables;
#[doc(hidden)]
pub mod predict;
#[doc(hidden)]
pub mod prob_tables;
#[doc(hidden)]
pub mod quant;
#[doc(hidden)]
pub mod scan;
/// AVX2 SIMD kernels (inter prediction, loop filter, inverse transforms); x86_64-only, see
/// the hub `src/simd.rs` and its submodules.
#[cfg(target_arch = "x86_64")]
#[doc(hidden)]
pub mod simd;
#[doc(hidden)]
pub mod subpel;
#[doc(hidden)]
pub mod superframe;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[doc(hidden)]
pub mod tile;
#[doc(hidden)]
pub mod transform;
#[cfg(test)]
#[path = "../tests/unit/support.rs"]
mod unit_test_support;

// VENDOR(uefi-player): core/alloc paths work in both std and no_std builds.
use alloc::sync::Arc;
use alloc::{vec, vec::Vec};

use compressed_header::{
    parse_compressed_header, CompressedHeaderError, FrameContext, FrameContextStore,
};
use counts::{adapt_coef_probs, adapt_noncoef_probs};
use dpb::{Dpb, RefFrameData};
use header::{
    parse_uncompressed_header, ColorConfig, FrameHeader, FrameType, HeaderError, LoopFilterDeltas,
    PersistentState, SegFeaturePersist, CS_UNKNOWN,
};
use tile::{MiGrid, TileDecoder, TileError};

/// Error type covering everything that can cause [`Decoder::decode_frame`] to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Failed to parse the uncompressed header (`uncompressed_header`). Two variants are
    /// raised by the [`Decoder`] itself rather than the parser: `FrameSizeTooLarge` (a
    /// signaled size the decoder refuses to allocate) and `RefFrameFormatMismatch` (an
    /// inter frame whose reference doesn't share its bit depth / subsampling).
    Header(HeaderError),
    /// Failed to parse the compressed header (`compressed_header`).
    CompressedHeader(CompressedHeaderError),
    /// Failed to decode tiles, mode info, or tokens.
    Tile(TileError),
    /// Frame data is malformed, e.g. `header_size_in_bytes` exceeds the frame data length.
    TruncatedFrame,
    /// The DPB slot referenced by `show_existing_frame` has no frame stored
    /// (does not occur with normal conformance bitstreams).
    MissingReferenceFrame,
}

impl From<HeaderError> for DecodeError {
    fn from(e: HeaderError) -> Self {
        DecodeError::Header(e)
    }
}

impl From<CompressedHeaderError> for DecodeError {
    fn from(e: CompressedHeaderError) -> Self {
        DecodeError::CompressedHeader(e)
    }
}

impl From<TileError> for DecodeError {
    fn from(e: TileError) -> Self {
        DecodeError::Tile(e)
    }
}

/// One decoded plane's pixel data. 8-bit streams (`BitDepth == 8`) decode into `U8` (no
/// memory bloat from widening the overwhelmingly common case); 10/12-bit streams (VP9
/// profiles 2/3) decode into `U16` (each sample 0..=1023 or 0..=4095).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaneData {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl PlaneData {
    /// Number of samples (not bytes) -- e.g. `width * height` for a cropped plane, the same
    /// meaning regardless of variant.
    pub fn len(&self) -> usize {
        match self {
            PlaneData::U8(v) => v.len(),
            PlaneData::U16(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the samples as 8-bit. Panics if this plane is `U16` (10/12-bit) -- for
    /// consumers that only ever handle 8-bit output.
    pub fn as_u8(&self) -> &[u8] {
        match self {
            PlaneData::U8(v) => v,
            PlaneData::U16(_) => panic!("PlaneData::as_u8 called on a 16-bit (10/12-bit) plane"),
        }
    }
}

/// One decoded frame. Cropped to the display size (`FrameWidth`/`FrameHeight`), holding the
/// 3 Y/U/V planes as [`PlaneData`] (row-major). Chroma sizing follows
/// `subsampling_x`/`subsampling_y`: 4:2:0 for profiles 0/2; profiles 1/3 also output
/// 4:2:2, 4:4:0, or 4:4:4.
///
/// The `u`/`v` sizes follow the output process of spec §8.9, computed as
/// `((width + subsampling_x) >> subsampling_x) x ((height + subsampling_y) >> subsampling_y)`
/// (for 4:2:0 this is `((width+1)/2) x ((height+1)/2)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub y: PlaneData,
    pub u: PlaneData,
    pub v: PlaneData,
}

/// Crops one plane to `(w, h)` and narrows to `PlaneData::U8`/`U16` depending on `bit_depth`
/// (8-bit output stays byte-sized; 10/12-bit needs the full `u16` range).
fn plane_to_plane_data(plane: &framebuffer::Plane, w: usize, h: usize, bit_depth: u8) -> PlaneData {
    if bit_depth == 8 {
        PlaneData::U8(plane.crop_u8(w, h))
    } else {
        PlaneData::U16(plane.crop(w, h))
    }
}

/// Crops the frame buffer (`planes`) to the display size and builds a [`Frame`].
fn crop_to_frame(
    planes: &[framebuffer::Plane; 3],
    width: u32,
    height: u32,
    color_config: &ColorConfig,
) -> Frame {
    let sub_x = color_config.subsampling_x as u32;
    let sub_y = color_config.subsampling_y as u32;
    let uv_width = ((width + sub_x) >> sub_x) as usize;
    let uv_height = ((height + sub_y) >> sub_y) as usize;
    let bit_depth = color_config.bit_depth;

    Frame {
        width,
        height,
        bit_depth,
        subsampling_x: sub_x,
        subsampling_y: sub_y,
        y: plane_to_plane_data(&planes[0], width as usize, height as usize, bit_depth),
        u: plane_to_plane_data(&planes[1], uv_width, uv_height, bit_depth),
        v: plane_to_plane_data(&planes[2], uv_width, uv_height, bit_depth),
    }
}

/// Builds a [`RefFrameData`] for DPB storage using the same crop calculation as
/// [`crop_to_frame`] (spec §8.10 "Reference frame update process" step 1).
fn build_ref_frame_data(
    planes: &[framebuffer::Plane; 3],
    width: u32,
    height: u32,
    color_config: &ColorConfig,
) -> RefFrameData {
    let sub_x = color_config.subsampling_x as u32;
    let sub_y = color_config.subsampling_y as u32;
    let uv_width = ((width + sub_x) >> sub_x) as usize;
    let uv_height = ((height + sub_y) >> sub_y) as usize;

    RefFrameData {
        width,
        height,
        subsampling_x: sub_x,
        subsampling_y: sub_y,
        bit_depth: color_config.bit_depth,
        y: planes[0].crop_to_plane(width as usize, height as usize),
        u: planes[1].crop_to_plane(uv_width, uv_height),
        v: planes[2].crop_to_plane(uv_width, uv_height),
    }
}

/// Read-only per-frame decode statistics, recorded purely for observation (e.g. test
/// assertions that a stream actually exercised a given decode path). Has no effect on
/// decode behavior. See [`DecodedFrame::info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDecodeInfo {
    /// `intra_only` (spec §6.2). `false` for key frames.
    pub intra_only: bool,
    /// `FrameIsIntra` (spec §6.2). `true` for key frames and intra-only frames.
    pub frame_is_intra: bool,
    /// `reset_frame_context` (spec §7.2). Always 0 for key frames and when
    /// `error_resilient_mode == 1`.
    pub reset_frame_context: u8,
    /// `segmentation_enabled` (spec §6.2.11).
    pub segmentation_enabled: bool,
    /// Indexed by `header::SEG_LVL_*`. `seg_features_active[level]` is `true` if
    /// `segmentation_enabled` and `FeatureEnabled[segment][level]` is set for any of
    /// the `MAX_SEGMENTS` segments in this frame.
    pub seg_features_active: [bool; header::SEG_LVL_MAX],
}

fn ref_frame_data_to_frame(data: &RefFrameData) -> Frame {
    Frame {
        width: data.width,
        height: data.height,
        bit_depth: data.bit_depth,
        subsampling_x: data.subsampling_x,
        subsampling_y: data.subsampling_y,
        y: plane_to_plane_data(&data.y, data.y.width, data.y.height, data.bit_depth),
        u: plane_to_plane_data(&data.u, data.u.width, data.u.height, data.bit_depth),
        v: plane_to_plane_data(&data.v, data.v.width, data.v.height, data.bit_depth),
    }
}

/// The outcome of decoding one constituent VP9 frame of a container chunk; one element
/// of [`Decoder::decode_frame`]'s result.
///
/// The two fields are `Option` independently because the three kinds of constituent
/// frame populate them differently:
/// - a shown decoded frame (`show_frame == 1`): `info: Some`, `frame: Some`
/// - a hidden decoded frame (`show_frame == 0`, e.g. an altref): `info: Some`, `frame: None`
/// - `show_existing_frame == 1`: `info: None` (no uncompressed header is parsed, so
///   there are no stats to report), `frame: Some` (the referenced DPB slot's pixels)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Decode statistics, for observation only (see [`FrameDecodeInfo`]).
    pub info: Option<FrameDecodeInfo>,
    /// The displayable picture, cropped to display size. `None` for hidden frames.
    pub frame: Option<Frame>,
}

/// A stateful decoder for decoding multiple frames in sequence.
///
/// VP9 carries the following state across frames, so frames cannot be processed
/// independently one by one:
/// - Reference frame slots (`RefFrameWidth`/`RefFrameHeight`, spec §6.2.5
///   `frame_size_with_refs`) and the actual pixel data ([`Dpb`], spec §8.10).
/// - Frame contexts (the 4 probability table slots selected by `frame_context_idx`,
///   spec §7.1.2 `load_probs`/`save_probs`). See [`FrameContextStore`].
/// - The previous frame's `Mvs`/`RefFrames`, referenced when `UsePrevFrameMvs`
///   (spec §7.2.6) is true (this implementation keeps the previous frame's
///   [`MiGrid`] in its entirety).
/// - The loop filter's `ref_deltas`/`mode_deltas` (spec §7.2, reset only by
///   `setup_past_independence()`).
/// - `LastFrameType` (spec §7.2; used in the `updateFactor` calculation for
///   probability adaptation, spec §8.4.3).
pub struct Decoder {
    /// Cross-frame state spec §7.2 requires: `RefFrameWidth`/`RefFrameHeight`, the loop
    /// filter's `ref_deltas`/`mode_deltas`, and the segmentation feature state -- see
    /// [`PersistentState`].
    persist: PersistentState,
    frame_contexts: FrameContextStore,
    /// Used for computing `UsePrevFrameMvs` (spec §7.2.6). Not updated on
    /// `show_existing_frame` (because `compute_image_size` isn't called).
    prev_frame_dims: Option<(u32, u32)>,
    prev_show_frame: Option<bool>,
    /// The previous frame's `Mvs`/`RefFrames` (equivalent to `PrevMvs`/`PrevRefFrames`).
    /// `Arc`-wrapped so handing a read-only copy to next frame's `TileDecoder` (when
    /// `UsePrevFrameMvs`) is a refcount bump rather than a deep clone.
    prev_mi_grid: Option<Arc<MiGrid>>,
    /// Inter frames and intra-only frames (`Profile == 0`) don't resend
    /// `color_config` in the bitstream, so the value is carried over from the
    /// most recent keyframe/intra-only frame (spec §7.2: it's a conformance
    /// requirement that bit depth and subsampling match the reference frame).
    last_color_config: Option<ColorConfig>,
    /// The actual pixel data of reference frames (spec §8.10 `FrameStore`).
    dpb: Dpb,
    /// `PrevSegmentIds` (spec §6.4.14), row-major `MiRows x MiCols` (unpadded).
    /// Cleared to all-zero on the first frame and whenever the frame size changes
    /// (spec §7.2.6), updated after decoding only when
    /// `segmentation_enabled && segmentation_update_map` (spec §8.1 step 3). `Arc`-wrapped
    /// so the per-frame clone-in to `TileDecoder` (unconditional, unlike `prev_mi_grid`) is
    /// a refcount bump rather than a deep clone.
    prev_segment_ids: Arc<Vec<u8>>,
    /// `LastFrameType` (spec §7.2). Not updated on `show_existing_frame` frames.
    last_frame_type: Option<FrameType>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            persist: PersistentState::default(),
            frame_contexts: FrameContextStore::new(),
            prev_frame_dims: None,
            prev_show_frame: None,
            prev_mi_grid: None,
            last_color_config: None,
            dpb: Dpb::new(),
            prev_segment_ids: Arc::new(Vec::new()),
            last_frame_type: None,
        }
    }

    /// Resets `PrevSegmentIds` to all-zero when the spec requires it, before tile decode
    /// (an error-resilient inter frame reads the map during its own decode):
    /// - `compute_image_size()` step 1 (spec §7.2.6): first invocation or
    ///   FrameWidth/FrameHeight changed since the previous invocation (detected via
    ///   `prev_frame_dims`, read before `decode_frame` overwrites it at the end).
    /// - `setup_past_independence()` (spec §7.2): `FrameIsIntra || error_resilient_mode`
    ///   (the caller passes this as `setup_past_independence`).
    fn clear_prev_segment_ids_if_needed(
        &mut self,
        dims: (u32, u32),
        image_size: &header::ImageSize,
        setup_past_independence: bool,
    ) {
        if setup_past_independence || self.prev_frame_dims != Some(dims) {
            self.prev_segment_ids = Arc::new(vec![
                0u8;
                (image_size.mi_cols * image_size.mi_rows)
                    as usize
            ]);
        }
    }

    /// Spec §8.1 step 3: `PrevSegmentIds` is refreshed from this frame's `SegmentIds` (read
    /// out of `grid`) only when segmentation_enabled && segmentation_update_map (not gated by
    /// show_frame). Otherwise it is left as-is (already reset to zero by
    /// [`Self::clear_prev_segment_ids_if_needed`] if the size changed).
    fn refresh_prev_segment_ids(
        &mut self,
        header: &header::NewFrameHeader,
        image_size: &header::ImageSize,
        grid: &MiGrid,
    ) {
        if header.segmentation.enabled && header.segmentation.update_map {
            let mut new_map = vec![0u8; (image_size.mi_cols * image_size.mi_rows) as usize];
            for row in 0..image_size.mi_rows {
                for col in 0..image_size.mi_cols {
                    new_map[(row * image_size.mi_cols + col) as usize] =
                        grid.get(row, col).segment_id;
                }
            }
            self.prev_segment_ids = Arc::new(new_map);
        }
    }

    /// Reference frame update process (spec §8.10). A zero refresh mask changes no DPB slot,
    /// so avoid cropping/copying all three planes only to discard them.
    fn refresh_reference_frames(
        &mut self,
        header: &header::NewFrameHeader,
        color_config: &ColorConfig,
        planes: &[framebuffer::Plane; 3],
    ) {
        if header.refresh_frame_flags == 0 {
            return;
        }

        // Arc-wrapped so `Dpb::update` shares this frame's pixel data across every refreshed
        // slot instead of deep-cloning it per slot (up to 8x on a keyframe).
        let ref_data = Arc::new(build_ref_frame_data(
            planes,
            header.width,
            header.height,
            color_config,
        ));
        self.dpb.update(header.refresh_frame_flags, &ref_data);
        for (slot, size) in self.persist.ref_frame_sizes.iter_mut().enumerate() {
            if (header.refresh_frame_flags >> slot) & 1 == 1 {
                *size = (header.width, header.height);
            }
        }
    }

    /// Decodes one container chunk (one IVF frame, one WebM block, etc.). Callers must pass
    /// chunks extracted from an IVF or similar container in bitstream order (decode order),
    /// not display order.
    ///
    /// A chunk may pack more than one VP9 frame via the "superframe" mechanism, so this
    /// splits it (`superframe::split_superframe`) and decodes each contained VP9 frame in
    /// turn, returning one [`DecodedFrame`] per constituent frame in bitstream order
    /// (`show_existing_frame` chunks, which never carry a superframe index, yield exactly
    /// one element). At most one element carries `frame: Some` -- when several
    /// constituents are marked shown (spatial-SVC streams mark every layer shown), only
    /// the last of them keeps its picture, matching libvpx's display behavior; the
    /// others still report their `info`. Internal state (reference frame buffers, frame
    /// context, previous frame's MVs, etc.) is updated for every constituent frame,
    /// shown or hidden.
    pub fn decode_frame(&mut self, chunk: &[u8]) -> Result<Vec<DecodedFrame>, DecodeError> {
        let mut decoded = Vec::new();
        for frame_data in superframe::split_superframe(chunk) {
            decoded.push(self.decode_one_frame(frame_data)?);
        }
        // Spec §5.26 permits a superframe to produce multiple output frames ("it is also legal
        // for a superframe to result in multiple output frames"), so §8.9's per-frame Output
        // process, read in isolation, would surface every constituent whose own show_frame is
        // 1. In practice this crate's conformance oracle (libvpx, via ffmpeg -- see
        // docs/implementation-notes.md "M4 wave 3") surfaces exactly one displayed frame per
        // chunk: each shown constituent overwrites a pending-output slot, so the LAST SHOWN
        // constituent survives (spatial-SVC streams mark every layer shown; only the top layer
        // is displayed). Mirror that: only the last constituent with a decoded picture keeps
        // its `frame`; earlier ones keep their `info` (decode stats) but have `frame` cleared.
        // Keying on last-SHOWN rather than positionally-last means a (conforming but unusual)
        // chunk ending in a hidden frame cannot lose its visible output. No-op for the common
        // case (at most one shown constituent per chunk).
        if let Some(last_shown) = decoded.iter().rposition(|df| df.frame.is_some()) {
            for df in &mut decoded[..last_shown] {
                df.frame = None;
            }
        }
        Ok(decoded)
    }

    /// Decodes exactly one VP9 frame (not a container chunk -- see [`Decoder::decode_frame`],
    /// the public entry point, which splits a chunk into these via
    /// `superframe::split_superframe`).
    ///
    /// See [`DecodedFrame`] for how `info`/`frame` reflect the three kinds of constituent
    /// frame. Internal state (reference frame buffers, frame context, previous frame's
    /// MVs, etc.) is updated whether or not the frame is shown.
    fn decode_one_frame(&mut self, frame_data: &[u8]) -> Result<DecodedFrame, DecodeError> {
        let _total_t = bench_timing::StageTimer::start(bench_timing::Stage::Total);
        let (parsed, consumed) = {
            let _t = bench_timing::StageTimer::start(bench_timing::Stage::HeaderParse);
            parse_uncompressed_header(frame_data, &self.persist)?
        };
        let header = match parsed {
            FrameHeader::New(h) => h,
            FrameHeader::ShowExistingFrame {
                frame_to_show_map_idx,
            } => {
                let data = self
                    .dpb
                    .get(frame_to_show_map_idx)
                    .ok_or(DecodeError::MissingReferenceFrame)?;
                return Ok(DecodedFrame {
                    info: None,
                    frame: Some(ref_frame_data_to_frame(data)),
                });
            }
        };

        // header.color_config is Some exactly when frame_is_intra (see header.rs); resolve it
        // once, here, to the value that applies to this frame. Reproduces the old
        // fabricate-then-patch behavior exactly, including its degenerate-case fallback (an
        // inter frame appearing before any intra frame ever ran): the same fixed placeholder
        // color config that header.rs used to fabricate for every non-intra frame.
        let color_config = header.color_config.unwrap_or_else(|| {
            self.last_color_config.unwrap_or(ColorConfig {
                bit_depth: 8,
                color_space: CS_UNKNOWN,
                color_range: false,
                subsampling_x: 1,
                subsampling_y: 1,
            })
        });
        if header.frame_is_intra {
            self.last_color_config = Some(color_config);
        }

        let header_size = header.header_size_in_bytes as usize;
        let compressed_start = consumed;
        let compressed_end = compressed_start
            .checked_add(header_size)
            .ok_or(DecodeError::TruncatedFrame)?;
        if compressed_end > frame_data.len() {
            return Err(DecodeError::TruncatedFrame);
        }
        let compressed_bytes = &frame_data[compressed_start..compressed_end];

        // UsePrevFrameMvs (spec §7.2.6).
        let use_prev_frame_mvs = !header.frame_is_intra
            && !header.error_resilient_mode
            && self.prev_frame_dims == Some((header.width, header.height))
            && self.prev_show_frame == Some(true);

        // Reject an absurd frame size before it reaches the buffer allocations below (which
        // use `vec![]` and would abort the process on failure, not return an error). A
        // malformed header can signal up to 65536x65536; see `MAX_FRAME_LUMA_SAMPLES`.
        if header.width as u64 * header.height as u64 > header::MAX_FRAME_LUMA_SAMPLES {
            return Err(DecodeError::Header(HeaderError::FrameSizeTooLarge));
        }

        let image_size = header::compute_image_size(header.width, header.height);
        self.clear_prev_segment_ids_if_needed(
            (header.width, header.height),
            &image_size,
            header.frame_is_intra || header.error_resilient_mode,
        );

        // setup_past_independence() (spec §7.2): called only when FrameIsIntra ||
        // error_resilient_mode. Its `save_probs` calls reset 0, 1, or all 4 stored
        // frame context slots to defaults depending on reset_frame_context (see
        // `frame_context_reset`). The load/save below use `effective_frame_context_idx()`,
        // which is 0 in this branch; the raw `frame_context_idx` passed here is the
        // pre-forcing value the `reset_frame_context == 2` `save_probs` targets.
        if header.frame_is_intra || header.error_resilient_mode {
            match frame_context_reset(
                header.frame_type,
                header.error_resilient_mode,
                header.reset_frame_context,
                header.frame_context_idx,
            ) {
                FrameContextReset::All => self.frame_contexts.reset_all(),
                FrameContextReset::Slot(idx) => {
                    self.frame_contexts.save(idx, FrameContext::default())
                }
                FrameContextReset::None => {}
            }
        }
        // The starting value equivalent to `load_probs`/`load_probs2` (spec §6.1, start of
        // `frame()`). Kept around after the compressed_header call because `refresh_probs()`
        // (spec §6.1.2) restores it to this pre-forward-update value before applying
        // backward adaptation.
        let starting_probs = self
            .frame_contexts
            .load(header.effective_frame_context_idx());

        let compressed = {
            let _t = bench_timing::StageTimer::start(bench_timing::Stage::CompressedHeader);
            parse_compressed_header(compressed_bytes, &header, starting_probs.clone())?
        };

        // Resolve the pixel data of the DPB slots this frame references, for motion
        // compensation (spec §8.5.2.3-8.5.2.4). Not referenced when `FrameIsIntra == 1`.
        // `get_arc` is a refcount bump, not a deep copy of the referenced frame's pixels.
        let resolved_refs: [Option<Arc<RefFrameData>>; 3] = if header.frame_is_intra {
            [None, None, None]
        } else {
            // VENDOR(uefi-player): core path works in both std and no_std builds.
            core::array::from_fn(|i| self.dpb.get_arc(header.ref_frame_idx[i]))
        };

        // Spec §8.5.1 / libvpx `valid_ref_frame_img_fmt`: an inter frame's references must share
        // its bit depth and chroma subsampling. Reject a stream mixing sample formats (the
        // `mixedrefcsp` vector) here rather than mis-predicting across them below.
        for r in resolved_refs.iter().flatten() {
            if r.bit_depth != color_config.bit_depth
                || r.subsampling_x != color_config.subsampling_x as u32
                || r.subsampling_y != color_config.subsampling_y as u32
            {
                return Err(DecodeError::Header(HeaderError::RefFrameFormatMismatch));
            }
        }
        // NOTE: reference *sizes* are deliberately NOT validated here. Spec §8.5.2.3's 2x
        // scaling bound applies per reference a block actually uses, and a conformant stream
        // may list an out-of-range slot it never predicts from (3-layer spatial SVC: a base-
        // layer frame's ref_frame_idx can point at a 4x-larger enhancement-layer frame --
        // `vp90-2-22-svc_1280x720_3` does). The bound is enforced at use time in
        // `decode_block` (`TileError::RefFrameSizeOutOfRange`).

        let tile_data = &frame_data[compressed_end..];
        let prev_grid = if use_prev_frame_mvs {
            self.prev_mi_grid.clone()
        } else {
            None
        };
        let mut tile_decoder = TileDecoder::new_with_prev(
            &header,
            color_config,
            &compressed,
            use_prev_frame_mvs,
            prev_grid,
            resolved_refs,
            self.prev_segment_ids.clone(),
        );
        {
            let _t = bench_timing::StageTimer::start(bench_timing::Stage::TileDecode);
            tile_decoder.decode_tiles(tile_data)?;
        }
        // Spec §8.1 step 2: "If loop_filter_level is not equal to 0, the loop filter
        // process ... is invoked" -- the whole process (including §8.8.1's frame init,
        // which can raise a per-block level above 0 via loop_filter_ref_deltas even when
        // the frame-level loop_filter_level is 0) is gated on this frame-level value, not
        // just the per-edge computed level.
        if header.loop_filter.level != 0 {
            let _t = bench_timing::StageTimer::start(bench_timing::Stage::LoopFilter);
            tile_decoder.apply_loop_filter(&header.loop_filter);
        }
        // Remainder of decode_one_frame (PrevSegmentIds refresh, probability adaptation,
        // DPB update, output Frame construction) is timed as one "DpbOutput" bucket; this
        // timer lives to the end of the function (no more fallible `?` calls follow).
        let _dpb_t = bench_timing::StageTimer::start(bench_timing::Stage::DpbOutput);

        self.refresh_prev_segment_ids(&header, &image_size, tile_decoder.mi_grid());

        // refresh_probs() (spec §6.1.2).
        let final_probs = refresh_probs(
            &header,
            starting_probs,
            &compressed,
            tile_decoder.counts(),
            self.last_frame_type,
        );
        if header.refresh_frame_context {
            self.frame_contexts
                .save(header.effective_frame_context_idx(), final_probs);
        }

        self.refresh_reference_frames(&header, &color_config, tile_decoder.planes());

        // compute_image_size is never called for show_existing_frame, so by the time we
        // reach here it has always been called (spec §7.2.6). Recorded for UsePrevFrameMvs.
        self.prev_frame_dims = Some((header.width, header.height));
        self.prev_show_frame = Some(header.show_frame);
        self.persist.loop_filter_deltas = LoopFilterDeltas {
            ref_deltas: header.loop_filter.ref_deltas,
            mode_deltas: header.loop_filter.mode_deltas,
        };
        self.persist.segmentation = SegFeaturePersist {
            enabled: header.segmentation.feature_enabled,
            data: header.segmentation.feature_data,
            abs_or_delta: header.segmentation.abs_or_delta_update,
        };
        self.last_frame_type = Some(header.frame_type);
        let info = FrameDecodeInfo {
            intra_only: header.intra_only,
            frame_is_intra: header.frame_is_intra,
            reset_frame_context: header.reset_frame_context,
            segmentation_enabled: header.segmentation.enabled,
            seg_features_active: core::array::from_fn(|level| {
                header.segmentation.enabled
                    && header
                        .segmentation
                        .feature_enabled
                        .iter()
                        .any(|seg| seg[level])
            }),
        };

        let frame = if header.show_frame {
            Some(crop_to_frame(
                tile_decoder.planes(),
                header.width,
                header.height,
                &color_config,
            ))
        } else {
            None
        };
        // Last use of tile_decoder: hand back its finished MiGrid by value (no clone-out)
        // now that every other accessor (planes(), counts(), mi_grid() above) has run.
        self.prev_mi_grid = Some(Arc::new(tile_decoder.into_mi_grid()));
        Ok(DecodedFrame {
            info: Some(info),
            frame,
        })
    }
}

/// Which stored frame context slot(s) get reset to defaults, per the `save_probs`
/// calls inside `setup_past_independence()` (spec §7.2). Only meaningful when called
/// under `FrameIsIntra || error_resilient_mode` (the condition under which
/// `setup_past_independence()` runs at all; see the call site in `decode_frame`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameContextReset {
    /// `reset_frame_context` is 0 or 1: no stored context is touched.
    None,
    /// `reset_frame_context == 2`: only the slot at this (raw, pre-`setup_past_independence`) index is reset.
    Slot(u8),
    /// `frame_type == KEY_FRAME`, `error_resilient_mode`, or `reset_frame_context == 3`: all 4 slots are reset.
    All,
}

/// `frame_context_idx` must be the raw bitstream value (`NewFrameHeader::frame_context_idx`, not
/// `effective_frame_context_idx()`), since `save_probs( frame_context_idx )` runs before
/// `setup_past_independence()` forces the effective index to 0.
fn frame_context_reset(
    frame_type: FrameType,
    error_resilient_mode: bool,
    reset_frame_context: u8,
    frame_context_idx: u8,
) -> FrameContextReset {
    if frame_type == FrameType::KeyFrame || error_resilient_mode || reset_frame_context == 3 {
        FrameContextReset::All
    } else if reset_frame_context == 2 {
        FrameContextReset::Slot(frame_context_idx)
    } else {
        FrameContextReset::None
    }
}

/// `refresh_probs()` (spec §6.1.2): the frame context to save back after a frame's decode --
/// a pure function of the frame's header, the pre-forward-update starting probabilities
/// (`load_probs`' value), the parsed compressed header, the tile decode's adaptation counts,
/// and the previous frame's type (spec §8.4.3's updateFactor). Backward adaptation runs only
/// when neither error_resilient_mode nor frame_parallel_decoding_mode is set; otherwise the
/// post-forward-update probabilities are saved as-is.
fn refresh_probs(
    header: &header::NewFrameHeader,
    starting_probs: FrameContext,
    compressed: &compressed_header::CompressedHeader,
    counts: &counts::Counts,
    last_frame_type: Option<FrameType>,
) -> FrameContext {
    if !header.error_resilient_mode && !header.frame_parallel_decoding_mode {
        // load_probs( frame_context_idx ): restore all tables except tx_probs/skip_prob
        // to their pre-forward-update value (starting_probs). tx_probs/skip_prob are
        // left at their post-forward-update value from compressed_header(). The two
        // fields are copied aside (24B + 3B) before moving starting_probs into working,
        // rather than cloning the whole multi-KB struct just to overwrite most of it.
        let pre_update_tx_probs = starting_probs.tx_probs;
        let pre_update_skip_prob = starting_probs.skip_prob;
        let mut working = starting_probs;
        working.tx_probs = compressed.probs.tx_probs;
        working.skip_prob = compressed.probs.skip_prob;

        // Spec §8.4.3: determining updateFactor.
        let update_factor = if header.frame_is_intra {
            112
        } else if last_frame_type == Some(FrameType::KeyFrame) {
            128
        } else {
            112
        };
        adapt_coef_probs(&mut working.coef_probs, counts, update_factor);

        if !header.frame_is_intra {
            // load_probs2( frame_context_idx ): also restore tx_probs/skip_prob to
            // their pre-forward-update value before applying adapt_noncoef_probs.
            working.tx_probs = pre_update_tx_probs;
            working.skip_prob = pre_update_skip_prob;
            adapt_noncoef_probs(
                &mut working,
                counts,
                header.interpolation_filter,
                compressed.tx_mode,
                header.allow_high_precision_mv,
            );
        }
        working
    } else {
        (*compressed.probs).clone()
    }
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
