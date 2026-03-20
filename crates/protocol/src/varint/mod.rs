#![allow(clippy::module_name_repetitions)]

//! Variable-length integer codec for the rsync wire protocol.
//!
//! Variable-length integers appear repeatedly in the rsync protocol, most
//! notably when exchanging compatibility flags once both peers have agreed on a
//! protocol version. The routines in this module mirror upstream `io.c`
//! implementations (`read_varint()`/`write_varint()`) so higher layers can
//! serialise and parse these values without depending on the original C code.
//!
//! The codec exposes a streaming API via [`read_varint`] and [`write_varint`],
//! plus helpers for working with in-memory buffers. The lookup table that maps
//! tag prefixes to the number of continuation bytes is copied directly from
//! upstream, ensuring byte-for-byte equivalence with rsync 3.4.1.
//!
//! # Examples
//!
//! Encode a set of compatibility flags into a `Vec<u8>` and decode the result
//! without touching an I/O object:
//!
//! ```
//! use protocol::{decode_varint, encode_varint_to_vec};
//!
//! let mut encoded = Vec::new();
//! encode_varint_to_vec(255, &mut encoded);
//! let (value, remainder) = decode_varint(&encoded).expect("varint decoding succeeds");
//! assert_eq!(value, 255);
//! assert!(remainder.is_empty());
//! ```
//!
//! # See also
//!
//! - [`crate::compatibility::CompatibilityFlags`] for the compatibility flag
//!   bitfield that relies on this codec.

mod decode;
mod encode;
mod table;

#[cfg(test)]
mod tests;

pub use decode::{
    decode_varint, read_int, read_longint, read_varint, read_varint30_int, read_varlong,
    read_varlong30,
};
pub use encode::{
    encode_varint_to_vec, write_int, write_longint, write_varint, write_varint30_int,
    write_varlong, write_varlong30,
};
