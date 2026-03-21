//! Compression strategy trait definition.

use super::CompressionAlgorithmKind;
use std::io;

/// Strategy trait for compression operations.
///
/// Implementations provide algorithm-specific compression and decompression
/// while exposing a uniform interface for callers.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent usage.
///
/// # Example
///
/// ```
/// use compress::strategy::{CompressionStrategy, ZlibStrategy};
/// use compress::zlib::CompressionLevel;
///
/// let strategy = ZlibStrategy::new(CompressionLevel::Default);
/// let mut compressed = Vec::new();
/// let bytes = strategy.compress(b"hello world", &mut compressed).unwrap();
/// assert!(bytes > 0);
/// ```
pub trait CompressionStrategy: Send + Sync {
    /// Compresses the input data and appends it to the output vector.
    ///
    /// Returns the number of compressed bytes written to `output`.
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize>;

    /// Decompresses the input data and appends it to the output vector.
    ///
    /// Returns the number of decompressed bytes written to `output`.
    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize>;

    /// Returns the algorithm kind for this strategy.
    fn algorithm_kind(&self) -> CompressionAlgorithmKind;

    /// Returns the human-readable algorithm name.
    fn algorithm_name(&self) -> &'static str {
        self.algorithm_kind().name()
    }
}
