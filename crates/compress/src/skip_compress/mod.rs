//! Compression tuning based on file type.
//!
//! This module implements intelligent compression decisions to avoid wasting CPU
//! on files that are already compressed or otherwise incompressible.
//!
//! # Design
//!
//! The compression decider uses a three-pronged approach:
//!
//! 1. **Extension-based detection**: Fast O(1) lookup for known file extensions
//! 2. **Magic byte detection**: Identify compressed files by their headers
//! 3. **Auto-detection**: Sample-based compression ratio analysis
//!
//! # Upstream Compatibility
//!
//! This module mirrors upstream rsync's `--skip-compress` functionality, which
//! allows users to specify file extensions that should not be compressed during
//! transfer. The default skip list includes common compressed formats.
//!
//! # Example
//!
//! ```
//! use compress::skip_compress::{CompressionDecider, CompressionDecision};
//! use std::path::Path;
//!
//! let decider = CompressionDecider::with_default_skip_list();
//!
//! // JPEG files are known to be incompressible
//! assert_eq!(
//!     decider.should_compress(Path::new("photo.jpg"), None),
//!     CompressionDecision::Skip
//! );
//!
//! // Unknown extensions without content return AutoDetect
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), None),
//!     CompressionDecision::AutoDetect
//! );
//!
//! // With content, the decider can make a decision
//! let text_content = b"Hello, world! This is some text.";
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), Some(text_content)),
//!     CompressionDecision::Compress
//! );
//! ```

mod adaptive;
mod decider;
mod defaults;
mod magic;
#[cfg(test)]
mod tests;
mod types;

pub use adaptive::AdaptiveCompressor;
pub use decider::CompressionDecider;
pub use magic::{KNOWN_SIGNATURES, MagicSignature};
pub use types::{CompressionDecision, FileCategory, Suffix};

/// Default size of the sample block for auto-detection (4 KB).
///
/// This size provides a good balance between detection accuracy and overhead.
/// Smaller samples may miss compression patterns, while larger samples waste CPU.
pub const DEFAULT_SAMPLE_SIZE: usize = 4 * 1024;

/// Default compression ratio threshold for auto-detection.
///
/// If the compressed size is >= 90% of the original size, the file is considered
/// incompressible. This threshold matches typical behavior where compression
/// overhead means ratios above 90% provide negligible benefit.
pub const DEFAULT_COMPRESSION_THRESHOLD: f64 = 0.90;
