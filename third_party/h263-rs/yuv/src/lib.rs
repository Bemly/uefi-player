//! Pure-rust BT.601 YUV color space support

#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
extern crate alloc;

pub mod bt601;
