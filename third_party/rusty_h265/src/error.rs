//! The crate's error type — dependency-free, hand-rolled `Display`.
//!
//! `Again` / `Eof` mirror FFmpeg's `EAGAIN` / `AVERROR_EOF` control-flow
//! convention (the same contract as `rusty_vp9`): they drive the push/pull
//! loop and are not failures.

// VENDOR(uefi-player): core::{result,fmt,error} + alloc::string work in both
// std and no_std builds (`String` is the same type either way; `write!`
// needs no extra import).
use alloc::string::String;

/// Crate-wide result alias.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors (and control-flow signals) produced by the decoder.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A code path that is scaffolded but not yet implemented.
    Unimplemented(&'static str),
    /// End of stream: all output has been drained.
    Eof,
    /// More input is required before output can be produced.
    Again,
    /// The bitstream is malformed or internally inconsistent.
    InvalidData(String),
    /// The input is valid but uses a feature outside this decoder's scope
    /// (RExt / SCC / multi-layer profiles are parsed and refused by name).
    Unsupported(String),
}

impl Error {
    /// `#[cold]` and out of line: `InvalidData` owns a `String`, so every
    /// `Error::invalid("...")` inlines an allocation and a copy of the literal
    /// at the call site. Those sites are bitstream-conformance checks that
    /// never fire on a valid stream, and four of them sit inside
    /// `residual_block`, the hottest function in the decoder. Every `&str`
    /// literal shares one monomorphisation, so this is a single out-of-line
    /// body for all of them -- and the enum's public shape is unchanged.
    #[cold]
    #[inline(never)]
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidData(msg.into())
    }
    #[cold]
    #[inline(never)]
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Unimplemented(what) => write!(f, "not yet implemented: {what}"),
            Error::Eof => write!(f, "end of stream"),
            Error::Again => write!(f, "more input required"),
            Error::InvalidData(msg) => write!(f, "invalid data: {msg}"),
            Error::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl core::error::Error for Error {}

impl From<crate::bits::OutOfData> for Error {
    fn from(_: crate::bits::OutOfData) -> Self {
        Error::InvalidData("bit reader ran out of data".into())
    }
}
