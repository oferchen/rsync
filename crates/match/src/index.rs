//! crates/match/src/index.rs
//!
//! Signature index for fast delta block lookup.

use std::collections::{HashMap, VecDeque};

use checksums::RollingDigest;

use signature::{FileSignature, SignatureAlgorithm, SignatureBlock};

/// Index over a file signature that accelerates delta matching.
#[derive(Clone, Debug)]
pub struct DeltaSignatureIndex {
    block_length: usize,
    strong_length: usize,
    algorithm: SignatureAlgorithm,
    blocks: Vec<SignatureBlock>,
    lookup: HashMap<(u16, u16, usize), Vec<usize>>,
}

impl DeltaSignatureIndex {
    /// Builds a signature index from the provided [`FileSignature`].
    ///
    /// The helper only indexes blocks that match the canonical block length
    /// reported by the layout. Files that produce fewer than one full block
    /// therefore return `None`, mirroring upstream rsync's behaviour of
    /// disabling the rolling checksum pipeline for very small payloads.
    #[must_use]
    pub fn from_signature(
        signature: &FileSignature,
        algorithm: SignatureAlgorithm,
    ) -> Option<Self> {
        let block_length = signature.layout().block_length().get() as usize;
        let strong_length = usize::from(signature.layout().strong_sum_length().get());
        let blocks: Vec<SignatureBlock> = signature.blocks().to_vec();

        let mut lookup: HashMap<(u16, u16, usize), Vec<usize>> = HashMap::new();
        let mut has_full_blocks = false;

        for (index, block) in blocks.iter().enumerate() {
            if block.len() != block_length {
                continue;
            }

            has_full_blocks = true;
            let digest = block.rolling();
            lookup
                .entry((digest.sum1(), digest.sum2(), block.len()))
                .or_default()
                .push(index);
        }

        if !has_full_blocks {
            return None;
        }

        Some(Self {
            block_length,
            strong_length,
            algorithm,
            blocks,
            lookup,
        })
    }

    /// Returns the canonical block length expressed in bytes.
    #[must_use]
    pub fn block_length(&self) -> usize {
        self.block_length
    }

    /// Returns the strong checksum length used by the signature.
    #[must_use]
    pub fn strong_length(&self) -> usize {
        self.strong_length
    }

    /// Returns the [`SignatureBlock`] for the provided index.
    #[must_use]
    pub fn block(&self, index: usize) -> &SignatureBlock {
        &self.blocks[index]
    }

    /// Attempts to locate a matching block for a contiguous byte slice.
    pub fn find_match_bytes(&self, digest: RollingDigest, window: &[u8]) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        let key = (digest.sum1(), digest.sum2(), window.len());
        let candidates = self.lookup.get(&key)?;
        for &index in candidates {
            let block = &self.blocks[index];
            if block.len() != window.len() {
                continue;
            }
            let strong = self.algorithm.compute_truncated(window, self.strong_length);
            if strong.as_slice() == block.strong() {
                return Some(index);
            }
        }

        None
    }

    /// Attempts to locate a matching block for a non-contiguous window backed by a [`VecDeque`].
    pub fn find_match_window(
        &self,
        digest: RollingDigest,
        window: &VecDeque<u8>,
        scratch: &mut Vec<u8>,
    ) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        scratch.clear();
        let (front, back) = window.as_slices();
        scratch.extend_from_slice(front);
        scratch.extend_from_slice(back);
        self.find_match_bytes(digest, scratch.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ProtocolVersion;
    use signature::{SignatureLayoutParams, calculate_signature_layout, generate_file_signature};
    use std::num::NonZeroU8;

    #[test]
    fn from_signature_returns_none_without_full_blocks() {
        let params = SignatureLayoutParams::new(
            64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let data = vec![0u8; 64];
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        assert!(DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).is_none());
    }

    #[test]
    fn find_match_bytes_locates_full_block() {
        let data = vec![b'a'; 1500];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let digest = index.block(0).rolling();
        let window = vec![b'a'; index.block_length()];
        let found = index.find_match_bytes(digest, &window).expect("match");
        assert_eq!(found, 0);
    }

    #[test]
    fn find_match_window_handles_split_buffers() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let digest = index.block(0).rolling();
        let mut window = VecDeque::with_capacity(index.block_length());
        let mut scratch = Vec::with_capacity(index.block_length());
        for &byte in &data[..index.block_length()] {
            window.push_back(byte);
        }
        // Rotate the deque to force a split backing store.
        for _ in 0..5 {
            let byte = window.pop_front().unwrap();
            window.push_back(byte);
        }

        let found = index
            .find_match_window(digest, &window, &mut scratch)
            .expect("match");
        assert_eq!(found, 0);
    }

    #[test]
    fn delta_signature_index_block_length() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        assert!(index.block_length() > 0);
    }

    #[test]
    fn delta_signature_index_strong_length() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        assert_eq!(index.strong_length(), 16);
    }

    #[test]
    fn delta_signature_index_block_accessor() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let block = index.block(0);
        assert_eq!(block.len(), index.block_length());
    }

    #[test]
    fn delta_signature_index_clone() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");
        let cloned = index.clone();

        assert_eq!(index.block_length(), cloned.block_length());
        assert_eq!(index.strong_length(), cloned.strong_length());
    }

    #[test]
    fn delta_signature_index_debug() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let debug = format!("{index:?}");
        assert!(debug.contains("DeltaSignatureIndex"));
    }

    #[test]
    fn find_match_bytes_wrong_length_returns_none() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let digest = index.block(0).rolling();
        // Window with wrong length
        let window = vec![b'a'; index.block_length() - 1];
        assert!(index.find_match_bytes(digest, &window).is_none());
    }

    #[test]
    fn find_match_bytes_no_match_returns_none() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let digest = index.block(0).rolling();
        // Window with right length but different content
        let window = vec![b'z'; index.block_length()];
        assert!(index.find_match_bytes(digest, &window).is_none());
    }

    #[test]
    fn find_match_window_wrong_length_returns_none() {
        let data = vec![b'a'; 2048];
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let digest = index.block(0).rolling();
        let mut window = VecDeque::new();
        let mut scratch = Vec::new();
        // Add fewer bytes than block length
        for _ in 0..index.block_length() - 1 {
            window.push_back(b'a');
        }
        assert!(
            index
                .find_match_window(digest, &window, &mut scratch)
                .is_none()
        );
    }
}
