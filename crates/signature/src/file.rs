//! crates/signature/src/file.rs
//!
//! Aggregated file signature container.

use crate::block::SignatureBlock;
use crate::layout::SignatureLayout;

/// Aggregated signature for a file produced by [`crate::generate_file_signature`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSignature {
    layout: SignatureLayout,
    blocks: Vec<SignatureBlock>,
    total_bytes: u64,
}

impl FileSignature {
    /// Creates a new signature container.
    pub(crate) const fn new(
        layout: SignatureLayout,
        blocks: Vec<SignatureBlock>,
        total_bytes: u64,
    ) -> Self {
        Self {
            layout,
            blocks,
            total_bytes,
        }
    }

    /// Creates a signature from raw components (for wire protocol reconstruction).
    #[must_use]
    pub const fn from_raw_parts(
        layout: SignatureLayout,
        blocks: Vec<SignatureBlock>,
        total_bytes: u64,
    ) -> Self {
        Self::new(layout, blocks, total_bytes)
    }

    /// Returns the layout used to generate the signature.
    #[inline]
    #[must_use]
    pub const fn layout(&self) -> SignatureLayout {
        self.layout
    }

    /// Returns the list of block entries in the order they were generated.
    #[inline]
    #[must_use]
    pub fn blocks(&self) -> &[SignatureBlock] {
        &self.blocks
    }

    /// Returns the total number of bytes consumed while generating the signature.
    #[inline]
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::SignatureLayout;
    use std::num::{NonZeroU8, NonZeroU32};

    fn test_layout() -> SignatureLayout {
        SignatureLayout::from_raw_parts(
            NonZeroU32::new(700).unwrap(),
            50,
            1,
            NonZeroU8::new(16).unwrap(),
        )
    }

    #[test]
    fn from_raw_parts() {
        let layout = test_layout();
        let blocks = vec![];
        let sig = FileSignature::from_raw_parts(layout, blocks.clone(), 100);
        assert_eq!(sig.layout(), layout);
        assert_eq!(sig.blocks(), &blocks);
        assert_eq!(sig.total_bytes(), 100);
    }

    #[test]
    fn clone_signature() {
        let layout = test_layout();
        let sig = FileSignature::from_raw_parts(layout, vec![], 50);
        let cloned = sig.clone();
        assert_eq!(sig, cloned);
    }

    #[test]
    fn debug_signature() {
        let layout = test_layout();
        let sig = FileSignature::from_raw_parts(layout, vec![], 50);
        let debug = format!("{sig:?}");
        assert!(debug.contains("FileSignature"));
    }
}
