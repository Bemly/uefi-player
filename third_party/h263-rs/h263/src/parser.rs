//! H.263 Bitstream parser functions.

mod block;
mod gob;
mod macroblock;
mod picture;
mod reader;
mod vlc;

pub use block::decode_block;
pub use gob::decode_gob;
pub use macroblock::decode_macroblock;
pub use picture::decode_picture;
// VENDOR(uefi-player): no_std port — `ByteSource` is the `std::io::Read`
// replacement every parser bound is written against (see reader.rs).
pub use reader::{ByteSource, H263Reader};
