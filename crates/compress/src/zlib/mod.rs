#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! Raw deflate compression helpers for rsync wire protocol compatibility.
//!
//! This module uses raw deflate format (no zlib header/trailer) to match
//! upstream rsync's compression wire format. [`CountingZlibEncoder`] accepts
//! incremental input while tracking the number of bytes produced by the
//! compressor so higher layers can report accurate compressed sizes without
//! buffering the resulting payload in memory. The complementary
//! [`CountingZlibDecoder`] wraps a reader that produces decompressed bytes
//! while recording how much output has been yielded so far, keeping the
//! counter accurate for both scalar and vectored reads so downstream bandwidth
//! accounting mirrors upstream behaviour.
//!
//! # Wire Format
//!
//! Raw deflate produces a bare DEFLATE stream without the 2-byte zlib header
//! or 4-byte Adler-32 checksum trailer. This matches rsync's `deflateInit2()`
//! call with `windowBits = -MAX_WBITS` (negative value indicates raw deflate).
//!
//! # Examples
//!
//! Compress data incrementally and obtain the compressed length:
//!
//! ```
//! use compress::zlib::{CompressionLevel, CountingZlibEncoder};
//! use std::io::Write;
//!
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
//! encoder.write(b"payload").unwrap();
//! let compressed_len = encoder.finish().unwrap();
//! assert!(compressed_len > 0);
//! ```
//!
//! Obtain a compressed buffer, stream it through
//! [`CountingZlibDecoder`], and collect the decompressed output:
//!
//! ```
//! use compress::zlib::{
//!     compress_to_vec, CompressionLevel, CountingZlibDecoder, CountingZlibEncoder,
//! };
//! use std::io::Read;
//!
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Best);
//! encoder.write(b"highly compressible payload").unwrap();
//! let compressed_len = encoder.finish().unwrap();
//!
//! let compressed = compress_to_vec(b"highly compressible payload", CompressionLevel::Best)
//!     .unwrap();
//! let mut decoder = CountingZlibDecoder::new(&compressed[..]);
//! let mut decoded = Vec::new();
//! decoder.read_to_end(&mut decoded).unwrap();
//! assert_eq!(decoded, b"highly compressible payload");
//! assert_eq!(decoder.bytes_read(), decoded.len() as u64);
//! assert!(compressed_len as usize <= compressed.len());
//! ```
//!
//! Forward compressed bytes into a caller-provided sink while tracking the
//! compressed length:
//!
//! ```
//! use compress::zlib::{CompressionLevel, CountingZlibEncoder};
//! use std::io::Write;
//!
//! let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
//! encoder.write_all(b"payload").unwrap();
//! let (compressed, bytes) = encoder.finish_into_inner().unwrap();
//! assert_eq!(bytes as usize, compressed.len());
//! ```

mod decoder;
mod encoder;
mod helpers;
mod level;

#[cfg(test)]
mod tests;

pub use decoder::CountingZlibDecoder;
pub use encoder::CountingZlibEncoder;
pub use helpers::{compress_to_vec, decompress_to_vec};
pub use level::{CompressionLevel, CompressionLevelError};
