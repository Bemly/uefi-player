//! Pure-rust BT.601 YUV color space support

#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): no_std port. `std` (default on) preserves upstream;
// without it this crate is `#![no_std]` + `alloc`. The `simd` feature keeps
// the wide integer kernel in the no_std shape (wide itself is `#![no_std]`),
// runtime-gated by `bt601::set_simd_enabled`.
extern crate alloc;

pub mod bt601;
