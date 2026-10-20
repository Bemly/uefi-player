//! Decoded picture buffer (DPB) management (spec §8.10
//! "Reference frame update process" + the `show_existing_frame` branch of §8.9 "Output process").
//!
//! Holds 8 slots' worth of the spec's `FrameStore[ i ][ plane ]`/`RefFrameWidth[ i ]`/
//! `RefFrameHeight[ i ]`/`RefSubsamplingX[ i ]`/`RefSubsamplingY[ i ]`/`RefBitDepth[ i ]`.
//! `FrameStore` holds pixel data already cropped to the display size (`FrameWidth`/
//! `FrameHeight`, chroma after subsampling); this matches the spec §8.10 copy range
//! being `x = 0..FrameWidth-1` and so on. This makes the clamping used in motion
//! compensation (spec §8.5.2.4's `lastX`/`lastY`) line up directly with the plane's
//! `width - 1`/`height - 1`.

// VENDOR(uefi-player): core/alloc paths work in both std and no_std builds.
use alloc::sync::Arc;

use crate::framebuffer::Plane;
use crate::prob_tables::NUM_REF_FRAMES;

/// One slot's worth of reference frame data (the spec's `FrameStore[ i ]` + accompanying size info).
/// Held behind `Arc` everywhere it's stored/passed (see [`Dpb`]) so that a frame's pixel data
/// is built once and shared, rather than deep-cloned per DPB slot / per consuming reference.
#[derive(Debug)]
pub struct RefFrameData {
    /// `RefFrameWidth[ i ]` (frame width, in luma terms).
    pub width: u32,
    /// `RefFrameHeight[ i ]`.
    pub height: u32,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub bit_depth: u8,
    /// Cropped (`width x height`) luma plane.
    pub y: Plane,
    /// Cropped (`((width+subX)>>subX) x ((height+subY)>>subY)`) chroma plane.
    pub u: Plane,
    pub v: Plane,
}

/// The DPB, with 8 slots.
#[derive(Debug, Clone)]
pub struct Dpb {
    slots: [Option<Arc<RefFrameData>>; NUM_REF_FRAMES],
}

impl Default for Dpb {
    fn default() -> Self {
        Self::new()
    }
}

impl Dpb {
    pub fn new() -> Self {
        Self {
            // VENDOR(uefi-player): core path works in both std and no_std builds.
            slots: core::array::from_fn(|_| None),
        }
    }

    pub fn get(&self, idx: u8) -> Option<&RefFrameData> {
        self.slots[idx as usize].as_deref()
    }

    /// Same slot as [`Dpb::get`], but returns a cheap `Arc` clone rather than a borrow, for
    /// callers that need to hold onto the data past `self`'s lifetime (e.g. resolving a
    /// frame's reference set for `TileDecoder`, which outlives the per-frame `&Dpb` borrow).
    pub fn get_arc(&self, idx: u8) -> Option<Arc<RefFrameData>> {
        self.slots[idx as usize].clone()
    }

    /// Step 1 of `Reference frame update process` (spec §8.10).
    /// Stores an `Arc` clone of `data` into every slot whose bit is set in
    /// `refresh_frame_flags` (the caller builds the frame's pixel data once; this only
    /// bumps a refcount per slot, not a deep copy).
    pub fn update(&mut self, refresh_frame_flags: u8, data: &Arc<RefFrameData>) {
        for (slot, entry) in self.slots.iter_mut().enumerate() {
            if (refresh_frame_flags >> slot) & 1 == 1 {
                *entry = Some(data.clone());
            }
        }
    }
}
