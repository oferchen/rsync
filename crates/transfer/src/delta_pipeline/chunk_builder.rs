//! Wire-token to [`DeltaChunk`] adapter for the parallel receive-delta path.
//!
//! Closes the receiver-side verify gap left open by BR-3i.c (#4640): the
//! `ParallelDeltaApplier` grew a real `verify_chunk` that compares the
//! computed strong digest of `chunk.data` against the chunk's
//! `expected_strong` field, but the receiver had no way to populate that
//! field for COPY tokens. Without a populated `expected_strong`, the verify
//! step silently degrades to "compute digest, skip comparison" for every
//! basis-match chunk, so corrupted basis bytes can never be caught before
//! they reach the destination writer.
//!
//! # Wiring
//!
//! The basis signature the receiver already negotiated and sent to the
//! sender holds the authoritative strong digest for every basis block.
//! When the wire decoder emits a [`DeltaToken::BlockRef`] (the receiver-side
//! equivalent of upstream's `COPY` token), the referenced block's strong
//! digest is already in memory in the [`FileSignature`] passed to
//! [`ChunkBuilder::next_chunk`]. We look it up by index and stamp it into
//! [`DeltaChunk::expected_strong`] via the BR-3i.c builder added on the
//! applier side. The strong digest is *not* re-derived from the basis
//! bytes; using the signature's digest is what lets a corrupted basis fail
//! verification.
//!
//! Literal tokens leave `expected_strong = None`: the sender does not
//! embed per-literal digests, so there is nothing to verify against. The
//! applier still computes the digest for parity with the verified path
//! (see `ParallelDeltaApplier::verify_chunk` in
//! `crates/engine/src/concurrent_delta/parallel_apply.rs`) but skips the
//! comparison, exactly as BR-3i.c documents.
//!
//! # Upstream Reference
//!
//! - `token.c:285` `simple_recv_token()` - the wire decoder this adapter
//!   layers on top of.
//! - `match.c:288` `find_match()` - sender-side block matcher; the strong
//!   digest at that site is the same one we compare against here.
//!
//! # Scope
//!
//! This module is wire-protocol-free: it does not touch the network reader,
//! the decompressor dictionary, or the disk-commit thread. It is a pure
//! function from `(FileSignature, DeltaToken, basis-bytes, sequence)` to
//! `Option<DeltaChunk>`, which keeps the per-chunk verify decision local
//! to the receiver-side staging structure and leaves the live token loop
//! in `crate::transfer_ops::token_loop` untouched until the production
//! pipeline migrates onto `ParallelDeltaApplier`.

use checksums::strong::strategy::ChecksumDigest;
use engine::concurrent_delta::{DeltaChunk, FileNdx};
use signature::FileSignature;

use crate::token_reader::DeltaToken;

/// Errors produced while turning a [`DeltaToken`] into a [`DeltaChunk`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChunkBuilderError {
    /// A `BlockRef` token referenced a basis-block index that does not
    /// exist in the file's signature. Always a malformed-stream condition
    /// the receiver must abort on; never the basis file's fault.
    #[error(
        "chunk builder: block index {index} out of bounds (signature has {block_count} blocks)"
    )]
    BlockIndexOutOfBounds {
        /// The offending block index from the wire token.
        index: usize,
        /// Total number of blocks in the negotiated signature.
        block_count: usize,
    },
    /// A `BlockRef` token was paired with a basis-bytes slice whose length
    /// does not match the basis block's recorded length. The receiver
    /// would otherwise feed mismatched bytes into the verify step, which
    /// would surface as a [`ChecksumMismatch`] for the wrong reason.
    ///
    /// [`ChecksumMismatch`]: engine::concurrent_delta::ParallelApplyError
    #[error(
        "chunk builder: basis-bytes length {got} does not match block {index} length {expected}"
    )]
    BasisLenMismatch {
        /// Block index from the wire token.
        index: usize,
        /// Length the signature records for the referenced block.
        expected: usize,
        /// Length of the basis-bytes slice the caller supplied.
        got: usize,
    },
}

/// Builds [`DeltaChunk`] values from wire delta tokens, populating
/// `expected_strong` from the negotiated basis signature for COPY tokens.
///
/// One builder lives for the duration of a single file's delta apply. The
/// caller bumps the sequence counter once per produced chunk, mirroring the
/// per-file `chunk_sequence` invariant that
/// `ParallelDeltaApplier::apply_one_chunk` documents.
///
/// # Per-file lifetime
///
/// The builder borrows the signature for the duration of the apply loop,
/// avoiding any clone of the per-block strong digests. The signature must
/// outlive every chunk the builder produces, which matches the receiver
/// pipeline's lifecycle (the basis signature is held until the file's
/// final commit).
///
/// # File-boundary drain contract (PIP-9.b.4)
///
/// After the last chunk for a file has been submitted to the applier, the
/// receiver must call `ParallelDeltaApplier::finish_file` before moving
/// to the next file. `finish_file` bakes in a [`flush_workers`] barrier
/// that waits for all in-flight chunks to drain, so callers never need to
/// call `flush_workers` separately. This ensures that the per-file writer
/// has received every byte in sequence before it is reclaimed for the
/// temp-file commit, checksum verification, and metadata application.
///
/// [`flush_workers`]: engine::concurrent_delta::ParallelDeltaApplier::flush_workers
#[derive(Debug)]
pub struct ChunkBuilder<'a> {
    ndx: FileNdx,
    signature: &'a FileSignature,
    next_sequence: u64,
}

impl<'a> ChunkBuilder<'a> {
    /// Creates a builder for a single file's delta apply.
    ///
    /// `ndx` is the [`FileNdx`] the `ParallelDeltaApplier` uses to route
    /// chunks to the per-file writer; the receiver pipeline derives this
    /// from the wire NDX it assigns when it opens the destination temp
    /// file.
    #[must_use]
    pub fn new(ndx: impl Into<FileNdx>, signature: &'a FileSignature) -> Self {
        Self {
            ndx: ndx.into(),
            signature,
            next_sequence: 0,
        }
    }

    /// Returns the next per-file sequence number the builder will stamp.
    #[must_use]
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Returns the file index every chunk this builder emits will carry.
    #[must_use]
    pub fn file_ndx(&self) -> FileNdx {
        self.ndx
    }

    /// Converts a literal-data token into a [`DeltaChunk`].
    ///
    /// `data` is the bytes the receiver already pulled off the wire (or
    /// decompressed for the `-z` path). Literal chunks carry no expected
    /// digest: the sender does not embed one and the applier skips the
    /// comparison.
    ///
    /// Bumps the per-file sequence counter by one.
    pub fn literal_chunk(&mut self, data: Vec<u8>) -> DeltaChunk {
        let chunk = DeltaChunk::literal(self.ndx, self.next_sequence, data);
        self.next_sequence += 1;
        chunk
    }

    /// Converts a basis-match (COPY-equivalent) token into a [`DeltaChunk`]
    /// with `expected_strong` populated from the signature.
    ///
    /// `basis_bytes` is the slice the receiver resolved from its
    /// memory-mapped basis file at the referenced block offset. The
    /// builder copies the strong digest out of the corresponding
    /// [`signature::SignatureBlock`] (the receiver computed and shipped it
    /// to the sender during signature exchange, so it is the authoritative
    /// expected value for `basis_bytes`).
    ///
    /// Bumps the per-file sequence counter by one on success.
    ///
    /// # Errors
    ///
    /// - [`ChunkBuilderError::BlockIndexOutOfBounds`] if `block_index` is
    ///   beyond the signature's block list. Always indicates the wire
    ///   stream is malformed; abort the file.
    /// - [`ChunkBuilderError::BasisLenMismatch`] if `basis_bytes.len()`
    ///   does not match the signature block's recorded length. Indicates
    ///   the receiver's basis-resolution step disagreed with the
    ///   signature; the verify step would otherwise mis-attribute the
    ///   failure to a checksum mismatch.
    pub fn matched_chunk(
        &mut self,
        block_index: usize,
        basis_bytes: Vec<u8>,
    ) -> Result<DeltaChunk, ChunkBuilderError> {
        let blocks = self.signature.blocks();
        let block = blocks
            .get(block_index)
            .ok_or(ChunkBuilderError::BlockIndexOutOfBounds {
                index: block_index,
                block_count: blocks.len(),
            })?;
        if basis_bytes.len() != block.len() {
            return Err(ChunkBuilderError::BasisLenMismatch {
                index: block_index,
                expected: block.len(),
                got: basis_bytes.len(),
            });
        }
        let expected = ChecksumDigest::new(block.strong());
        let chunk = DeltaChunk::matched(self.ndx, self.next_sequence, basis_bytes)
            .with_expected_strong(expected);
        self.next_sequence += 1;
        Ok(chunk)
    }

    /// Dispatches a [`DeltaToken`] to the matching [`DeltaChunk`] builder.
    ///
    /// Returns `Ok(None)` for [`DeltaToken::End`] - the end marker terminates
    /// the token stream rather than producing a chunk.
    ///
    /// `literal_data` and `basis_bytes` are the caller-resolved payloads
    /// for the respective token shapes; the builder leaves I/O to the
    /// receiver pipeline (`literal_to_buf` for literals, `MapFile::map_ptr`
    /// for basis bytes) and consumes the already-resolved buffers.
    ///
    /// # Errors
    ///
    /// Forwards any [`ChunkBuilderError`] from [`matched_chunk`](Self::matched_chunk)
    /// when the token is a `BlockRef`.
    pub fn next_chunk(
        &mut self,
        token: TokenForBuild,
    ) -> Result<Option<DeltaChunk>, ChunkBuilderError> {
        match token {
            TokenForBuild::Literal(data) => Ok(Some(self.literal_chunk(data))),
            TokenForBuild::BlockRef { index, basis_bytes } => {
                self.matched_chunk(index, basis_bytes).map(Some)
            }
            TokenForBuild::End => Ok(None),
        }
    }
}

/// Builder input: a [`DeltaToken`] paired with the receiver-resolved bytes.
///
/// Keeping the I/O outside the builder (the caller hands in already-read
/// literal bytes or already-mapped basis bytes) makes the builder a pure
/// function: it never touches the network reader or the `MapFile`, which
/// keeps testing straightforward and the per-chunk verify decision local
/// to the chunk shape.
#[derive(Debug)]
pub enum TokenForBuild {
    /// Literal payload resolved from the wire (or decompressed for `-z`).
    Literal(Vec<u8>),
    /// Basis-block reference paired with the bytes the receiver mapped
    /// from the basis file at the block's offset.
    BlockRef {
        /// Zero-based block index in the negotiated [`FileSignature`].
        index: usize,
        /// Basis bytes resolved from the receiver's mapped basis file.
        basis_bytes: Vec<u8>,
    },
    /// End-of-stream marker; the builder returns `None` and does not bump
    /// the sequence counter.
    End,
}

impl TokenForBuild {
    /// Pairs a [`DeltaToken::BlockRef`] with the basis bytes the receiver
    /// resolved for it.
    ///
    /// Provided as a convenience for callers that already have a
    /// [`DeltaToken`] in hand and only need the basis bytes lookup.
    /// `DeltaToken::Literal` is intentionally not handled here because
    /// the literal payload may still be in `LiteralData::Pending` shape,
    /// which requires a network read the builder is not aware of.
    ///
    /// # Errors
    ///
    /// Returns `None` when `token` is not a `BlockRef`; callers should
    /// use the `Literal` / `End` variants directly in that case.
    #[must_use]
    pub fn from_block_ref(token: &DeltaToken, basis_bytes: Vec<u8>) -> Option<Self> {
        match token {
            DeltaToken::BlockRef(index) => Some(Self::BlockRef {
                index: *index,
                basis_bytes,
            }),
            DeltaToken::Literal(_) | DeltaToken::End => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU8, NonZeroU32};
    use std::sync::Arc;

    use checksums::RollingDigest;
    use checksums::strong::strategy::{
        ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
    };
    use engine::concurrent_delta::ParallelDeltaApplier;
    use signature::{FileSignature, SignatureBlock, SignatureLayout};

    use super::*;

    /// Helper: builds a single-block MD5 signature whose stored strong
    /// digest matches `block_bytes`. Tests use this fixture to mimic the
    /// receiver's "I shipped this digest to the sender, so the sender's
    /// COPY token references a block whose expected digest is this value"
    /// invariant.
    fn make_md5_signature(block_bytes: &[u8]) -> (FileSignature, Arc<dyn ChecksumStrategy>) {
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0),
        );
        let strong = strategy.compute(block_bytes);
        let rolling = RollingDigest::from_bytes(block_bytes);
        let block = SignatureBlock::from_raw_parts(0, rolling, strong.as_bytes());
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(block_bytes.len().max(1) as u32).unwrap(),
            block_bytes.len() as u32,
            1,
            NonZeroU8::new(16).unwrap(),
        );
        let sig = FileSignature::from_raw_parts(layout, vec![block], block_bytes.len() as u64);
        (sig, strategy)
    }

    /// Helper: builds a signature whose stored digest deliberately does
    /// NOT match `block_bytes`. Used by the "corrupted basis" test to
    /// model a basis file whose bytes have drifted from the digest the
    /// receiver shipped.
    fn make_md5_signature_with_stale_digest(
        block_bytes: &[u8],
    ) -> (FileSignature, Arc<dyn ChecksumStrategy>) {
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0),
        );
        // Digest the receiver shipped is for a *different* block payload.
        let stale_strong = strategy.compute(b"the basis the receiver originally signed");
        let rolling = RollingDigest::from_bytes(block_bytes);
        let block = SignatureBlock::from_raw_parts(0, rolling, stale_strong.as_bytes());
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(block_bytes.len().max(1) as u32).unwrap(),
            block_bytes.len() as u32,
            1,
            NonZeroU8::new(16).unwrap(),
        );
        let sig = FileSignature::from_raw_parts(layout, vec![block], block_bytes.len() as u64);
        (sig, strategy)
    }

    #[test]
    fn receiver_populates_expected_strong_for_copy_chunks() {
        // BR-3i.d acceptance: a wire COPY (BlockRef) token must produce a
        // DeltaChunk whose `expected_strong` equals the signature's stored
        // strong digest for that block index.
        let block_bytes = vec![0xAA; 64];
        let (sig, strategy) = make_md5_signature(&block_bytes);

        let mut builder = ChunkBuilder::new(0u32, &sig);
        let chunk = builder
            .matched_chunk(0, block_bytes.clone())
            .expect("matched_chunk for in-bounds index");

        let expected = chunk
            .expected_strong
            .as_ref()
            .expect("expected_strong populated for copy chunks");
        assert_eq!(expected.as_bytes(), sig.blocks()[0].strong());
        // The digest the applier would compute from chunk.data must match
        // the populated expected digest, otherwise the test fixture is
        // self-contradictory.
        let recomputed = strategy.compute(&chunk.data);
        assert_eq!(expected.as_bytes(), recomputed.as_bytes());
        // Per-file sequence number monotonically bumped.
        assert_eq!(builder.next_sequence(), 1);
        assert!(!chunk.is_literal);
        assert_eq!(chunk.ndx, FileNdx::new(0));
        assert_eq!(chunk.chunk_sequence, 0);
    }

    #[test]
    fn receiver_leaves_expected_strong_none_for_literal_chunks() {
        // BR-3i.d acceptance: literal tokens carry no expected digest.
        // Senders do not embed per-literal digests, so the field must
        // stay `None` and the applier must skip comparison.
        let block_bytes = vec![0xBB; 32];
        let (sig, _strategy) = make_md5_signature(&block_bytes);

        let mut builder = ChunkBuilder::new(7u32, &sig);
        let chunk = builder.literal_chunk(b"raw literal bytes from the wire".to_vec());

        assert!(
            chunk.expected_strong.is_none(),
            "literal chunks must not carry an expected digest"
        );
        assert!(chunk.is_literal);
        assert_eq!(chunk.ndx, FileNdx::new(7));
        assert_eq!(chunk.chunk_sequence, 0);
        assert_eq!(builder.next_sequence(), 1);
    }

    #[test]
    fn end_to_end_corrupted_basis_fails_verify() {
        // BR-3i.d end-to-end: when the basis bytes the receiver mapped
        // disagree with the strong digest the signature records, the
        // ParallelDeltaApplier must surface a ChecksumMismatch and the
        // corrupted bytes must never reach the destination writer.
        let mapped_basis = vec![0xCC; 48];
        let (sig, strategy) = make_md5_signature_with_stale_digest(&mapped_basis);

        let applier = ParallelDeltaApplier::with_strategy(1, Arc::clone(&strategy));
        let sink: Vec<u8> = Vec::new();
        applier
            .register_file(11u32, Box::new(std::io::Cursor::new(sink)))
            .expect("register_file");

        let mut builder = ChunkBuilder::new(11u32, &sig);
        let chunk = builder
            .matched_chunk(0, mapped_basis)
            .expect("matched_chunk for in-bounds index");

        let err = applier
            .apply_one_chunk(chunk)
            .expect_err("verify must reject corrupted basis bytes");
        let msg = err.to_string();
        assert!(msg.contains("checksum mismatch"), "msg was: {msg}");
        assert!(msg.contains("ndx=11"), "msg was: {msg}");
        assert!(msg.contains("algorithm=md5"), "msg was: {msg}");

        // Writer remained untouched: verify happens before the per-file
        // mutex is taken, so a finish_file on the still-registered file
        // would observe zero bytes written.
        assert_eq!(
            applier
                .bytes_written(11u32)
                .expect("bytes_written for registered file"),
            0
        );
    }

    #[test]
    fn block_index_out_of_bounds_surfaces_typed_error() {
        let block_bytes = vec![0xDD; 32];
        let (sig, _strategy) = make_md5_signature(&block_bytes);

        let mut builder = ChunkBuilder::new(0u32, &sig);
        let err = builder
            .matched_chunk(7, block_bytes.clone())
            .expect_err("out-of-bounds block index must error");
        assert!(matches!(
            err,
            ChunkBuilderError::BlockIndexOutOfBounds {
                index: 7,
                block_count: 1
            }
        ));
        // The sequence counter must not bump on the error path.
        assert_eq!(builder.next_sequence(), 0);
    }

    #[test]
    fn basis_len_mismatch_surfaces_typed_error_before_verify() {
        // Defense in depth: catch the receiver's basis-resolution bug
        // before the applier sees mismatched bytes. Otherwise the apply
        // would surface as a ChecksumMismatch for the wrong reason.
        let block_bytes = vec![0xEE; 32];
        let (sig, _strategy) = make_md5_signature(&block_bytes);

        let mut builder = ChunkBuilder::new(0u32, &sig);
        // Receiver bug: mapped 31 bytes for a 32-byte block.
        let truncated = vec![0xEE; 31];
        let err = builder
            .matched_chunk(0, truncated)
            .expect_err("len mismatch must surface");
        assert!(matches!(
            err,
            ChunkBuilderError::BasisLenMismatch {
                index: 0,
                expected: 32,
                got: 31
            }
        ));
        assert_eq!(builder.next_sequence(), 0);
    }

    #[test]
    fn next_chunk_dispatches_each_variant() {
        let block_bytes = vec![0xFE; 16];
        let (sig, _strategy) = make_md5_signature(&block_bytes);

        let mut builder = ChunkBuilder::new(3u32, &sig);

        // Literal token: produces a literal chunk with no expected digest.
        let literal = builder
            .next_chunk(TokenForBuild::Literal(vec![1, 2, 3, 4]))
            .expect("literal dispatch")
            .expect("literal produces a chunk");
        assert!(literal.is_literal);
        assert!(literal.expected_strong.is_none());

        // BlockRef token: produces a matched chunk with expected digest.
        let matched = builder
            .next_chunk(TokenForBuild::BlockRef {
                index: 0,
                basis_bytes: block_bytes.clone(),
            })
            .expect("matched dispatch")
            .expect("matched produces a chunk");
        assert!(!matched.is_literal);
        let expected = matched
            .expected_strong
            .as_ref()
            .expect("expected_strong populated");
        assert_eq!(expected.as_bytes(), sig.blocks()[0].strong());

        // End token: produces no chunk and does not bump the counter.
        let before = builder.next_sequence();
        let none = builder
            .next_chunk(TokenForBuild::End)
            .expect("end dispatch");
        assert!(none.is_none());
        assert_eq!(builder.next_sequence(), before);
    }

    #[test]
    fn from_block_ref_filters_non_blockref_tokens() {
        let token = DeltaToken::BlockRef(2);
        let payload = vec![9u8; 4];
        let built = TokenForBuild::from_block_ref(&token, payload.clone())
            .expect("BlockRef tokens convert");
        match built {
            TokenForBuild::BlockRef { index, basis_bytes } => {
                assert_eq!(index, 2);
                assert_eq!(basis_bytes, payload);
            }
            _ => panic!("expected BlockRef variant"),
        }

        // Literal / End must return None so callers do not silently
        // mis-route them through the basis-bytes path.
        assert!(
            TokenForBuild::from_block_ref(
                &DeltaToken::Literal(crate::token_reader::LiteralData::Pending(4)),
                vec![]
            )
            .is_none()
        );
        assert!(TokenForBuild::from_block_ref(&DeltaToken::End, vec![]).is_none());
    }
}
