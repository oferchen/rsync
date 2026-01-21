//! LZ4 compression support.
//!
//! This module provides two LZ4 compression formats:
//!
//! - [`frame`]: Standard LZ4 frame format with magic bytes, checksums, and
//!   streaming support. Suitable for file compression and storage.
//!
//! - [`raw`]: Raw LZ4 blocks with rsync-specific 2-byte framing. Required for
//!   wire protocol compatibility with upstream rsync 3.4.1.
//!
//! # Wire Protocol Compatibility
//!
//! Upstream rsync uses raw LZ4 blocks (not frame format) with a custom 2-byte
//! header encoding the compressed size. Use the [`raw`] module for any code
//! that needs to interoperate with upstream rsync's compression.
//!
//! # Example
//!
//! ```
//! # #[cfg(feature = "lz4")]
//! # fn example() -> std::io::Result<()> {
//! use compress::lz4::{frame, raw};
//! use compress::zlib::CompressionLevel;
//!
//! // Frame format for local storage
//! let data = b"local file data";
//! let framed = frame::compress_to_vec(data, CompressionLevel::Default)?;
//! let restored = frame::decompress_to_vec(&framed)?;
//! assert_eq!(restored, data);
//!
//! // Raw format for wire protocol
//! let wire_data = b"wire transfer data";
//! let raw_compressed = raw::compress_block_to_vec(wire_data)?;
//! let wire_restored = raw::decompress_block_to_vec(&raw_compressed, wire_data.len())?;
//! assert_eq!(wire_restored, wire_data);
//! # Ok(())
//! # }
//! ```

pub mod frame;
pub mod raw;

// Re-export commonly used frame types at module level for backward compatibility
pub use frame::{CountingLz4Decoder, CountingLz4Encoder, compress_to_vec, decompress_to_vec};

use crate::algorithm::CompressionAlgorithm;

/// Returns the preferred compression algorithm when callers do not provide one explicitly.
#[must_use]
pub const fn default_algorithm() -> CompressionAlgorithm {
    CompressionAlgorithm::Lz4
}
