//! crates/signature/src/block.rs
//!
//! Individual signature block representation.

use checksums::RollingDigest;

/// Describes a single block within a file signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureBlock {
    index: u64,
    rolling: RollingDigest,
    strong: Vec<u8>,
}

impl SignatureBlock {
    /// Creates a new block descriptor.
    pub(crate) const fn new(index: u64, rolling: RollingDigest, strong: Vec<u8>) -> Self {
        Self {
            index,
            rolling,
            strong,
        }
    }

    /// Creates a block descriptor from raw components (for wire protocol reconstruction).
    #[must_use]
    pub const fn from_raw_parts(index: u64, rolling: RollingDigest, strong: Vec<u8>) -> Self {
        Self::new(index, rolling, strong)
    }

    /// Returns the zero-based index of the block within the signature.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// Returns the rolling checksum digest associated with the block.
    #[inline]
    #[must_use]
    pub const fn rolling(&self) -> RollingDigest {
        self.rolling
    }

    /// Returns the strong checksum bytes for the block.
    #[inline]
    #[must_use]
    pub fn strong(&self) -> &[u8] {
        &self.strong
    }

    /// Returns the number of bytes that contributed to the rolling checksum.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rolling.len()
    }

    /// Reports whether the block corresponds to an empty range.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_parts() {
        let rolling = RollingDigest::from_bytes(b"test");
        let strong = vec![1, 2, 3, 4];
        let block = SignatureBlock::from_raw_parts(42, rolling, strong.clone());
        assert_eq!(block.index(), 42);
        assert_eq!(block.rolling(), rolling);
        assert_eq!(block.strong(), &strong);
    }

    #[test]
    fn len_matches_rolling_digest_len() {
        let rolling = RollingDigest::from_bytes(b"hello world");
        let block = SignatureBlock::from_raw_parts(0, rolling, vec![]);
        assert_eq!(block.len(), 11);
    }

    #[test]
    fn is_empty_for_zero_length() {
        let rolling = RollingDigest::from_bytes(b"");
        let block = SignatureBlock::from_raw_parts(0, rolling, vec![]);
        assert!(block.is_empty());
    }

    #[test]
    fn is_not_empty_for_non_zero_length() {
        let rolling = RollingDigest::from_bytes(b"data");
        let block = SignatureBlock::from_raw_parts(0, rolling, vec![]);
        assert!(!block.is_empty());
    }

    #[test]
    fn clone_block() {
        let rolling = RollingDigest::from_bytes(b"test");
        let block = SignatureBlock::from_raw_parts(1, rolling, vec![1, 2, 3]);
        let cloned = block.clone();
        assert_eq!(block, cloned);
    }

    #[test]
    fn debug_block() {
        let rolling = RollingDigest::from_bytes(b"test");
        let block = SignatureBlock::from_raw_parts(0, rolling, vec![]);
        let debug = format!("{block:?}");
        assert!(debug.contains("SignatureBlock"));
    }
}
