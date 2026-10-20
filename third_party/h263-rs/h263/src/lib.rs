//! Pure-rust H.263 decoder

#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
extern crate alloc;

#[macro_use]
extern crate bitflags;

mod decoder;
mod error;
pub mod parser;
mod traits;
mod types;

pub use decoder::{DecoderOption, H263State};
pub use error::{Error, Result};
pub use types::{PictureOption, PictureTypeCode};
