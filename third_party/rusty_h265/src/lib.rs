//! **rusty_h265** — a pure-Rust HEVC / H.265 video decoder, no C, no FFI.
//!
//! Written from ITU-T H.265 (with the JCT-VC HM reference decoder as the
//! transcribable source) and gated on the JCT-VC HEVC_v1 conformance suite.
//! Scope (v1): Main, Main 10 and Main Still Picture — 4:2:0, 8 and 10 bit.
//! Range extensions, screen content coding and layered profiles are parsed
//! and refused with a named [`Error::Unsupported`].
//!
//! The push/pull API mirrors FFmpeg's send/receive convention:
//! [`Error::Again`] means "feed more input", [`Error::Eof`] means "drained".
//!
//! ```no_run
//! use rusty_h265::{Decoder, Error};
//!
//! fn decode(stream: &[u8]) -> Result<(), Error> {
//!     let mut dec = Decoder::new();
//!     dec.push_annexb(stream, None)?;
//!     dec.flush();
//!     while let Ok(frame) = dec.next_frame() {
//!         println!("{}x{} poc {}", frame.width, frame.height, frame.poc);
//!     }
//!     Ok(())
//! }
//! ```

#![forbid(unsafe_code)]
// VENDOR(uefi-player): no_std port (scalar). `std` (default on) preserves
// upstream; without it this crate is `#![no_std]` + `alloc`.
#![cfg_attr(not(feature = "std"), no_std)]
// VENDOR(uefi-player): always in scope; `use alloc::…` below resolves in both
// std and no_std builds (same pattern as upstream h264-common).
extern crate alloc;

/// Read an environment knob. Without `std` there is no environment, so this
/// is always `false` — the shipped default, which is what a chip runs.
#[cfg(feature = "std")]
pub(crate) fn knob(name: &str) -> bool {
    std::env::var_os(name).is_some()
}
/// Read an environment knob. Without `std` there is no environment, so this
/// is always `false` — the shipped default, which is what a chip runs.
#[cfg(not(feature = "std"))]
pub(crate) fn knob(_name: &str) -> bool {
    false
}

/// Diagnostic print. No-op without `std` (no stderr on the target); every
/// call site is already gated on a knob/trace flag, so nothing production
/// goes quiet that wasn't opt-in chatter.
#[cfg(not(feature = "std"))]
macro_rules! eprintln {
    ($($t:tt)*) => {{
        let _ = ::core::format_args!($($t)*);
    }};
}

mod error;
pub use error::{Error, Result};

pub mod bits;
pub mod cabac;
pub mod ctu;
pub mod decoder;
pub mod filters;
pub mod frame;
pub mod intra;
pub mod itx;
mod mcscratch;
pub mod md5;
pub mod nal;
pub mod pic;
#[cfg(feature = "prof")]
pub mod prof;

/// Time a stage, when built with the `prof` feature; expand to nothing
/// otherwise.
///
/// Defined HERE rather than in `prof` itself: `#[macro_export]` publishes a
/// macro from the module that defines it, so a macro defined inside a
/// `#[cfg(feature = ...)]` module does not exist at all in the configurations
/// that need it to expand to nothing. Every call site then fails to compile in
/// exactly the build that ships.
#[macro_export]
macro_rules! prof_scope {
    ($stage:expr) => {
        #[cfg(feature = "prof")]
        let _prof_guard = $crate::prof::Scope::new($stage);
    };
}
pub mod ps;
pub mod sei;
pub mod slice;
pub mod tables;

/// The kernel layer: instruction-set choice, and the deployment census.
pub use rusty_h265_accel as accel;

pub use decoder::{Decoder, Stats};
pub use frame::{Frame, Picture, Plane};

#[cfg(test)]
mod silent_zero_guards {
    //! Guards against values that read as a legitimate zero while actually
    //! meaning "nobody ever wrote this".
    //!
    //! A zero is the most dangerous value in an instrumented codec, because it
    //! is indistinguishable from a true measurement. This project has already
    //! paid for one: `PUT_WEIGHTED` was declared in the census and never
    //! incremented anywhere. It read 0, the 0 was taken as proof that explicit
    //! weighted prediction was an unreachable path, and a decision was recorded
    //! not to build a kernel for it. The path was in fact serving **100 % of
    //! motion compensation** on `WP_A_Toshiba_3` and 59 % of prediction writes
    //! on ordinary x265 content.
    //!
    //! Nothing failed. No test broke. The number was simply never written, and
    //! a never-written number looks exactly like a cold one.

    use std::path::{Path, PathBuf};

    fn rust_sources() -> Vec<PathBuf> {
        // `CARGO_MANIFEST_DIR` is crates/rusty_h265; its parent holds the
        // accel crate too, and between them they own every census site.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut out = Vec::new();
        let mut stack = vec![root.join("rusty_h265"), root.join("rusty_h265-accel")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Every counter the census declares must be handed to `bump`, `route` or
    /// `arm` somewhere that is not the declaration itself.
    #[test]
    fn every_census_counter_is_actually_incremented() {
        let files = rust_sources();
        assert!(files.len() > 5, "source walk found nothing; the guard would pass vacuously");

        let lib = files
            .iter()
            .find(|p| p.ends_with("rusty_h265-accel/src/lib.rs") || p.to_string_lossy().replace('\\', "/").ends_with("rusty_h265-accel/src/lib.rs"))
            .expect("accel lib.rs");
        let decl = std::fs::read_to_string(lib).unwrap();
        let start = decl.find("counters!(").expect("counters! macro");
        let end = decl[start..].find("\n    );").expect("end of counters!") + start;
        let names: Vec<String> = decl[start..end]
            .lines()
            .map(|l| l.trim().trim_end_matches(',').to_string())
            .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .collect();
        assert!(names.len() > 20, "expected the full census, found {}", names.len());

        // Every use OUTSIDE the declaration block.
        let mut bodies = String::new();
        for p in &files {
            let t = std::fs::read_to_string(p).unwrap_or_default();
            if std::ptr::eq(p, lib) {
                bodies.push_str(&t[..start]);
                bodies.push_str(&t[end..]);
            } else {
                bodies.push_str(&t);
            }
        }

        let dead: Vec<&String> = names.iter().filter(|n| !bodies.contains(n.as_str())).collect();
        assert!(
            dead.is_empty(),
            "census counters declared but never incremented — each reads 0 forever, \
             and a 0 is indistinguishable from a cold path: {dead:?}"
        );
    }

    /// The census must not be able to report an arm the dispatch did not take.
    ///
    /// The second instance of this bug in the same campaign: the weighted
    /// kernel's census predicate was computed *before* its bring-up switch was
    /// consulted, so with the switch on, the scalar arm still reported `SIMD`.
    /// A dispatcher that consults a switch must consult it in the census too.
    #[test]
    fn census_predicates_consult_the_same_switches_as_the_dispatch() {
        for p in rust_sources() {
            let t = std::fs::read_to_string(&p).unwrap_or_default();
            for (i, line) in t.lines().enumerate() {
                // A dispatch guarded by a bring-up switch...
                if line.contains("if ok") && line.contains("scalar_gate()") {
                    // ...must have a census predicate above it that also
                    // mentions the switch. Search the preceding 12 lines.
                    let lo = i.saturating_sub(12);
                    let window: String = t.lines().skip(lo).take(i - lo).collect::<Vec<_>>().join("\n");
                    if window.contains("let simd =") {
                        assert!(
                            window.contains("scalar_gate()"),
                            "{}:{}: dispatch consults `scalar_gate()` but the census \
                             predicate above it does not — the counter will report an \
                             arm that never ran",
                            p.display(),
                            i + 1
                        );
                    }
                }
            }
        }
    }
}
