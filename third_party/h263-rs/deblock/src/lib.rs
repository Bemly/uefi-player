//! Rust implementation of a deblocking filter inspired by ITU-T H.263 Annex J.
//! This is intended to be used as a postprocessing step, not as a loop filter.

#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
extern crate alloc;

pub mod deblock;
