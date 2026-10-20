//! Traits

use num_traits::{CheckedShl, CheckedShr, One, Zero};
// VENDOR(uefi-player): no_std port — `core` operator traits.
use core::ops::{BitAnd, BitOr, Not};

pub trait BitReadable:
    Copy
    + CheckedShl
    + CheckedShr
    + BitOr<Self, Output = Self>
    + BitAnd<Self, Output = Self>
    + Not<Output = Self>
    + Eq
    + Zero
    + One
    + From<u8>
{
}

impl<T> BitReadable for T where
    T: Copy
        + CheckedShl
        + CheckedShr
        + BitOr<Self, Output = Self>
        + BitAnd<Self, Output = Self>
        + Not<Output = Self>
        + Eq
        + Zero
        + One
        + From<u8>
{
}
