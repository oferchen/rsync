#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_compress` exposes compression primitives shared across the Rust rsync
//! workspace. The initial focus is parity with upstream rsync's zlib-based
//! compressor. Higher layers compose these helpers when negotiating `--compress`
//! sessions, allowing the same encoder/decoder implementations to be reused by
//! both client and daemon roles.
//!
//! # Design
//!
//! The crate currently provides the [`zlib`] module, which implements
//! streaming-friendly encoders and decoders built on top of
//! [`flate2`](https://docs.rs/flate2). The API emphasises incremental
//! processing: callers provide scratch buffers that are filled with compressed
//! or decompressed data while the internal state tracks totals for diagnostics
//! and progress reporting.
//!
//! # Invariants
//!
//! - Encoders and decoders never allocate internal output buffers. All output is
//!   written into the caller-provided vectors, allowing upper layers to reuse
//!   storage across files.
//! - Streams are finalised explicitly via [`zlib::CountingZlibEncoder::finish`],
//!   which emits trailer bytes and reports the final compressed length.
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
//! use rsync_compress::zlib::{CompressionLevel, CountingZlibEncoder, compress_to_vec, decompress_to_vec};
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
//! - [`zlib`] for the encoder/decoder implementation and API surface.
//! - `rsync_engine` for the transfer pipeline that integrates these helpers.

pub mod zlib;
