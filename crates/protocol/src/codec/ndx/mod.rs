//! NDX (file-list index) encoding and decoding for the rsync protocol.
//!
//! This module implements NDX encoding using the Strategy pattern to handle
//! the different wire formats between protocol versions:
//!
//! - **Protocol < 30 (Legacy)**: Simple 4-byte little-endian signed integers
//! - **Protocol >= 30 (Modern)**: Delta-encoded byte-reduction format
//!
//! # Strategy Pattern
//!
//! The `NdxCodec` trait defines the encoding/decoding interface, with two
//! implementations:
//! - `LegacyNdxCodec`: Protocol 28-29 (4-byte LE integers)
//! - `ModernNdxCodec`: Protocol 30+ (delta encoding)
//!
//! Use `create_ndx_codec` to get the appropriate codec for a protocol version.
//!
//! # Wire Formats
//!
//! ## Legacy (Protocol < 30)
//!
//! All NDX values are 4-byte little-endian signed integers:
//! - Positive file indices: direct value
//! - NDX_DONE (-1): `[0xFF, 0xFF, 0xFF, 0xFF]`
//! - Other negative values: direct value
//!
//! ## Modern (Protocol >= 30)
//!
//! Delta-encoded format for bandwidth efficiency:
//! - `0x00`: NDX_DONE (-1)
//! - `0xFF prefix`: negative values (other than -1)
//! - `1-253`: delta-encoded positive index
//! - `0xFE prefix`: extended encoding for larger indices
//!
//! # Upstream Reference
//!
//! - `io.c:2243-2287` - `write_ndx()` function
//! - `io.c:2289-2318` - `read_ndx()` function
//! - `rsync.h:285-288` - NDX constant definitions

mod codec;
mod constants;
mod goodbye;
mod state;

#[cfg(test)]
mod tests;

pub use codec::{
    LegacyNdxCodec, ModernNdxCodec, MonotonicNdxWriter, NdxCodec, NdxCodecEnum, create_ndx_codec,
};
pub use constants::{
    NDX_DEL_STATS, NDX_DONE, NDX_DONE_LEGACY_BYTES, NDX_DONE_MODERN_BYTE, NDX_FLIST_EOF,
    NDX_FLIST_OFFSET,
};
pub use goodbye::{read_goodbye, write_goodbye};
pub use state::{NdxState, write_ndx_done, write_ndx_flist_eof};
