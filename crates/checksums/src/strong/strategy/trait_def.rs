//! Strategy trait for runtime checksum algorithm dispatch.

use super::digest::ChecksumDigest;
use super::kind::ChecksumAlgorithmKind;

/// Trait for runtime-dispatched checksum computation.
///
/// Implementations provide algorithm-specific checksum computation while
/// exposing a uniform interface for callers.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent usage.
///
/// # Example
///
/// ```
/// use checksums::strong::strategy::{ChecksumStrategy, Md5Strategy};
///
/// let strategy = Md5Strategy::new();
/// let digest = strategy.compute(b"hello world");
/// assert_eq!(digest.len(), 16);
/// ```
pub trait ChecksumStrategy: Send + Sync {
    /// Computes the checksum digest for the input data.
    fn compute(&self, data: &[u8]) -> ChecksumDigest;

    /// Computes the checksum and writes it to the output buffer.
    ///
    /// The buffer must be at least [`digest_len()`](Self::digest_len) bytes.
    ///
    /// # Panics
    ///
    /// Panics if `out.len() < self.digest_len()`.
    fn compute_into(&self, data: &[u8], out: &mut [u8]) {
        let digest = self.compute(data);
        digest.copy_to(out);
    }

    /// Returns the digest length for this algorithm in bytes.
    fn digest_len(&self) -> usize;

    /// Returns the algorithm kind for this strategy.
    fn algorithm_kind(&self) -> ChecksumAlgorithmKind;

    /// Returns the human-readable algorithm name.
    fn algorithm_name(&self) -> &'static str {
        self.algorithm_kind().name()
    }
}
