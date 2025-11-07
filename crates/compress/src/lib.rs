#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `oc_rsync_compress` exposes compression primitives shared across the Rust rsync
//! workspace. The initial focus is parity with upstream rsync's zlib-based
//! compressor while progressively expanding to additional algorithms such as
//! Zstandard. Higher layers compose these helpers when negotiating `--compress`
//! sessions, allowing the same encoder/decoder implementations to be reused by
//! both client and daemon roles.
//!
//! # Design
//!
//! The crate currently provides the [`zlib`], [`lz4`], and [`zstd`] modules, which
//! implement streaming-friendly encoders and decoders built on top of
//! [`flate2`](https://docs.rs/flate2), [`lz4_flex`](https://docs.rs/lz4_flex), and
//! [`zstd`](https://docs.rs/zstd) respectively. The API emphasises
//! incremental processing: callers provide scratch buffers that are filled with
//! compressed or decompressed data while the internal state tracks totals for
//! diagnostics and progress reporting.
//!
//! # Invariants
//!
//! - Encoders and decoders never allocate internal output buffers. All output is
//!   written into the caller-provided vectors, allowing upper layers to reuse
//!   storage across files.
//! - Streams are finalised explicitly via
//!   [`zlib::CountingZlibEncoder::finish`], [`lz4::CountingLz4Encoder::finish`],
//!   and [`zstd::CountingZstdEncoder::finish`], which emit trailer bytes and
//!   report the final compressed length.
//! - Errors from the underlying zlib implementation are surfaced as
//!   [`std::io::Error`] values to integrate with the rest of the workspace.
//!
//! # Errors
//!
//! The encoder and decoder functions return [`std::io::Result`]. When zlib
//! reports an error the helper wraps it in
//! [`std::io::ErrorKind::Other`]. Callers can
//! surface these diagnostics via the central message facade in `rsync-core`.
//!
//! # Examples
//!
//! Compressing and decompressing a buffer with the streaming encoder and
//! convenience helpers:
//!
//! ```
//! use oc_rsync_compress::zlib::{CompressionLevel, CountingZlibEncoder, compress_to_vec, decompress_to_vec};
//!
//! # fn main() -> std::io::Result<()> {
//! let data = b"streaming example payload";
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
//! encoder.write(data)?;
//! let compressed_len = encoder.finish()?;
//! assert!(compressed_len > 0);
//!
//! let compressed = compress_to_vec(data, CompressionLevel::Default)?;
//! let decompressed = decompress_to_vec(&compressed)?;
//! assert_eq!(decompressed, data);
//! # Ok(())
//! # }
//! ```
//!
//! # See also
//!
//! - [`zlib`] for the zlib encoder/decoder implementation and API surface.
//! - [`lz4`] for the LZ4 frame encoder/decoder implementation.
//! - [`zstd`] for the Zstandard encoder/decoder implementation.
//! - `oc_rsync_engine` for the transfer pipeline that integrates these helpers.

pub mod algorithm;
mod common;
#[cfg(feature = "lz4")]
pub mod lz4;
pub mod zlib;
#[cfg(feature = "zstd")]
pub mod zstd;

pub use common::CountingSink;
