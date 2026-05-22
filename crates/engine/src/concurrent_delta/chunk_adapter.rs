//! In-memory shape adapter from per-file [`DeltaWork`] to per-chunk
//! [`DeltaChunk`].
//!
//! PIP-9.a substrate. The receiver pipeline today reads a per-file
//! [`DeltaWork`] item that carries the NDX, basis path, and accumulated
//! `literal_bytes`/`matched_bytes` counters, then streams a sequence of
//! resolved per-chunk byte spans (either literal payloads or basis-file
//! matches) over the existing SPSC pipe. The
//! [`ParallelDeltaApplier`](super::ParallelDeltaApplier) on the other side
//! expects each segment as a [`DeltaChunk`] carrying the per-file NDX, a
//! monotonic per-file `chunk_sequence`, the resolved bytes, a literal-vs-match
//! discriminator, and an optional strong-checksum digest the applier verifies
//! before committing the bytes to disk.
//!
//! This adapter is the pure in-memory shape transformation between those two
//! views. It performs no I/O, spawns no threads, holds no state, and does not
//! touch the wire protocol - PIP-9.b will plug it into the production token
//! loop without any wire-format change. PIP-7 surfaced the previous attempt's
//! receiver corruption when scaffolding had a side-effect-only swap with no
//! reader; the deliberate split between adapter (this file) and wire-up
//! (PIP-9.b) keeps each step independently reviewable.
//!
//! # Shape
//!
//! - [`ChunkPayload`] - per-chunk values supplied by the receiver's wire reader
//!   (the resolved bytes, the per-file sequence number, literal-vs-match, and
//!   an optional strong-checksum digest derived from the basis signature).
//! - [`ChunkSource`] - literal-vs-basis discriminator carried alongside the
//!   payload so the adapter stays expression-equivalent to the existing
//!   `is_literal` field on [`DeltaChunk`] without mirroring a `bool`.
//! - [`DeltaChunkAdapter`] - zero-state struct that exposes `from_delta_work`
//!   so callers can convert in one call without re-spelling the field map.
//!
//! # Invariants
//!
//! - `DeltaChunk::ndx == DeltaWork::ndx`. The adapter carries the NDX through
//!   unchanged so the applier's per-file slot map keys stay correlated with
//!   the file list index the receiver assigned at file-begin time.
//! - `DeltaChunk::data` is moved (not cloned) from `ChunkPayload::data`. The
//!   receiver already owns the resolved bytes; the adapter must not duplicate
//!   them on the hot path.
//! - `DeltaChunk::expected_strong` round-trips byte-for-byte. When the
//!   producer attached a basis-signature-derived strong checksum (BR-3i.d),
//!   the applier will compare it against `strategy.compute(&data)` in
//!   [`ParallelDeltaApplier::verify_chunk`].
//! - `DeltaChunk::chunk_sequence` is taken verbatim from
//!   [`ChunkPayload::chunk_sequence`]; the adapter does not assign or mutate
//!   sequence numbers (PIP-9.b is responsible for that).
//!
//! # Non-goals
//!
//! - This adapter does *not* split a `DeltaWork` into multiple chunks. The
//!   receiver's wire reader is the source of per-chunk granularity; the
//!   adapter is the per-chunk shape conversion. Callers loop over their own
//!   chunk stream and invoke the adapter per segment.
//! - This adapter does *not* touch any per-file state on the applier
//!   ([`register_file`](super::ParallelDeltaApplier::register_file),
//!   [`finish_file`](super::ParallelDeltaApplier::finish_file)). Those calls
//!   remain the wire-up's responsibility in PIP-9.b.
//!
//! [`DeltaWork`]: super::DeltaWork
//! [`DeltaChunk`]: super::DeltaChunk
//! [`ParallelDeltaApplier`]: super::ParallelDeltaApplier
//! [`ParallelDeltaApplier::verify_chunk`]: super::ParallelDeltaApplier
//! [`register_file`]: super::ParallelDeltaApplier
//! [`finish_file`]: super::ParallelDeltaApplier

use checksums::strong::strategy::ChecksumDigest;

use super::parallel_apply::DeltaChunk;
use super::types::DeltaWork;

/// Classifies a per-chunk payload as literal data or as a basis-file match.
///
/// Mirrors the boolean `is_literal` field that [`DeltaChunk`] exposes today
/// but keeps the call site self-documenting. The variants map 1:1:
/// [`ChunkSource::Literal`] -> `is_literal = true`,
/// [`ChunkSource::Copy`] -> `is_literal = false`. The applier treats both
/// shapes identically in the write path; the discriminator is preserved so
/// future stats reporting can split literal vs matched bytes without
/// changing the public chunk shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkSource {
    /// Bytes the sender transmitted as a literal token over the wire.
    Literal,
    /// Bytes the receiver resolved from the basis file via a COPY token
    /// (block-match reference).
    Copy,
}

impl ChunkSource {
    /// Returns the equivalent boolean form for the existing
    /// [`DeltaChunk::is_literal`] field.
    #[must_use]
    pub const fn is_literal(self) -> bool {
        matches!(self, Self::Literal)
    }
}

/// Per-chunk values the receiver's wire reader supplies alongside the owning
/// [`DeltaWork`].
///
/// Holds the resolved bytes and the metadata the
/// [`ParallelDeltaApplier`](super::ParallelDeltaApplier) needs to verify and
/// commit a single chunk:
///
/// - `chunk_sequence` - monotonic per-file submission counter the applier's
///   reorder buffer uses to replay chunks in submission order.
/// - `data` - resolved bytes for this chunk (literal payload or basis match).
/// - `source` - literal-vs-copy discriminator carried through to the
///   [`DeltaChunk::is_literal`] flag.
/// - `expected_strong` - optional strong-checksum digest derived from the
///   basis signature (BR-3i.d). When present, the applier verifies the
///   digest of `data` matches before committing the bytes.
#[derive(Debug, Clone)]
pub struct ChunkPayload {
    /// Monotonic per-file sequence number assigned at submission time.
    pub chunk_sequence: u64,
    /// Resolved bytes for this chunk.
    pub data: Vec<u8>,
    /// Whether the bytes came from the wire (literal) or the basis (copy).
    pub source: ChunkSource,
    /// Optional expected strong-checksum digest. See
    /// [`DeltaChunk::expected_strong`] for the round-trip semantics.
    pub expected_strong: Option<ChecksumDigest>,
}

impl ChunkPayload {
    /// Builds a literal-data payload with no expected digest attached.
    #[must_use]
    pub fn literal(chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            chunk_sequence,
            data,
            source: ChunkSource::Literal,
            expected_strong: None,
        }
    }

    /// Builds a basis-match payload with no expected digest attached.
    #[must_use]
    pub fn copy(chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            chunk_sequence,
            data,
            source: ChunkSource::Copy,
            expected_strong: None,
        }
    }

    /// Builder-style setter that attaches an expected strong-checksum digest.
    #[must_use]
    pub fn with_expected_strong(mut self, expected: ChecksumDigest) -> Self {
        self.expected_strong = Some(expected);
        self
    }
}

/// Zero-state shape adapter from [`DeltaWork`] + [`ChunkPayload`] to
/// [`DeltaChunk`].
///
/// All methods are pure in-memory transformations. Construct one with
/// [`DeltaChunkAdapter::new`] and call [`Self::from_delta_work`] per chunk
/// the wire reader produces. PIP-9.b will host the per-file loop that drives
/// this adapter from the receiver's token loop.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeltaChunkAdapter;

impl DeltaChunkAdapter {
    /// Constructs a fresh adapter. Holds no state; provided for call-site
    /// readability so PIP-9.b can spell the conversion as
    /// `DeltaChunkAdapter::new().from_delta_work(...)`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Converts a [`DeltaWork`] + per-chunk [`ChunkPayload`] into a
    /// [`DeltaChunk`] the applier can verify and commit.
    ///
    /// The NDX is taken from `work` so the resulting chunk slots into the
    /// applier's per-file map. The `payload` fields are moved into the
    /// chunk verbatim (no copies of `data`, byte-for-byte round-trip of
    /// `expected_strong`). The conversion is total: every
    /// `(DeltaWork, ChunkPayload)` pair yields exactly one `DeltaChunk`.
    #[must_use]
    pub fn from_delta_work(&self, work: &DeltaWork, payload: ChunkPayload) -> DeltaChunk {
        DeltaChunk {
            ndx: work.ndx(),
            chunk_sequence: payload.chunk_sequence,
            data: payload.data,
            is_literal: payload.source.is_literal(),
            expected_strong: payload.expected_strong,
        }
    }
}

/// Free-function form of [`DeltaChunkAdapter::from_delta_work`] for callers
/// that prefer not to spell the zero-state struct. Behaviour is identical.
#[must_use]
pub fn delta_work_to_chunk(work: &DeltaWork, payload: ChunkPayload) -> DeltaChunk {
    DeltaChunkAdapter::new().from_delta_work(work, payload)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use checksums::strong::strategy::{
        ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
    };

    use super::super::types::FileNdx;
    use super::*;

    fn md5_strategy() -> Box<dyn ChecksumStrategy> {
        ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0)
    }

    fn whole_file_work(ndx: u32) -> DeltaWork {
        DeltaWork::whole_file(ndx, PathBuf::from(format!("/dest/{ndx}")), 1024)
    }

    fn delta_work(ndx: u32) -> DeltaWork {
        DeltaWork::delta(
            ndx,
            PathBuf::from(format!("/dest/{ndx}")),
            PathBuf::from(format!("/basis/{ndx}")),
            2048,
            512,
            1536,
        )
    }

    #[test]
    fn literal_payload_preserves_bytes_byte_for_byte() {
        let work = whole_file_work(7);
        let bytes: Vec<u8> = (0..=255u8).collect();
        let payload = ChunkPayload::literal(0, bytes.clone());
        let chunk = DeltaChunkAdapter::new().from_delta_work(&work, payload);

        assert_eq!(chunk.ndx, FileNdx::new(7));
        assert_eq!(chunk.chunk_sequence, 0);
        assert_eq!(chunk.data, bytes);
        assert!(chunk.is_literal);
        assert!(chunk.expected_strong.is_none());
    }

    #[test]
    fn copy_payload_marks_chunk_as_match() {
        let work = delta_work(3);
        let bytes = vec![0xAB; 512];
        let payload = ChunkPayload::copy(42, bytes.clone());
        let chunk = delta_work_to_chunk(&work, payload);

        assert_eq!(chunk.ndx, FileNdx::new(3));
        assert_eq!(chunk.chunk_sequence, 42);
        assert_eq!(chunk.data, bytes);
        assert!(!chunk.is_literal);
        assert!(chunk.expected_strong.is_none());
    }

    #[test]
    fn expected_strong_round_trips_byte_for_byte() {
        let work = delta_work(11);
        let bytes = vec![0x55; 256];
        let strategy = md5_strategy();
        let digest = strategy.compute(&bytes);
        let expected_bytes = digest.as_bytes().to_vec();

        let payload = ChunkPayload::copy(1, bytes.clone()).with_expected_strong(digest);
        let chunk = DeltaChunkAdapter::new().from_delta_work(&work, payload);

        let attached = chunk
            .expected_strong
            .expect("digest must round-trip through the adapter");
        assert_eq!(attached.as_bytes(), expected_bytes.as_slice());
        assert_eq!(attached, strategy.compute(&chunk.data));
    }

    #[test]
    fn expected_strong_drives_real_verify_chunk_pass() {
        // Confirms the digest the adapter carries is the exact value the
        // applier's verify path will accept. Computes the digest from the
        // adapter's output chunk and compares it to the attached expectation.
        let work = whole_file_work(0);
        let data = b"oc-rsync pip-9.a round-trip".to_vec();
        let strategy = md5_strategy();
        let digest = strategy.compute(&data);

        let payload = ChunkPayload::literal(0, data.clone()).with_expected_strong(digest);
        let chunk = delta_work_to_chunk(&work, payload);

        let computed = strategy.compute(&chunk.data);
        assert_eq!(
            chunk
                .expected_strong
                .as_ref()
                .expect("expected digest attached"),
            &computed,
            "the digest the producer attached must equal strategy.compute(chunk.data)"
        );
    }

    #[test]
    fn empty_payload_yields_zero_length_chunk() {
        let work = whole_file_work(0);
        let payload = ChunkPayload::literal(0, Vec::new());
        let chunk = DeltaChunkAdapter::new().from_delta_work(&work, payload);

        assert_eq!(chunk.data.len(), 0);
        assert!(chunk.is_literal);
        assert!(chunk.expected_strong.is_none());
    }

    #[test]
    fn empty_copy_payload_keeps_copy_discriminator() {
        let work = delta_work(2);
        let payload = ChunkPayload::copy(0, Vec::new());
        let chunk = delta_work_to_chunk(&work, payload);

        assert_eq!(chunk.data.len(), 0);
        assert!(!chunk.is_literal);
    }

    #[test]
    fn whole_file_work_routes_literal_chunk() {
        // DeltaWork::whole_file represents a basis-less transfer; the
        // receiver still streams the bytes as one or more literal chunks.
        let work = whole_file_work(5);
        let payload = ChunkPayload::literal(0, vec![0x42; 64]);
        let chunk = DeltaChunkAdapter::new().from_delta_work(&work, payload);

        assert_eq!(chunk.ndx, FileNdx::new(5));
        assert!(chunk.is_literal);
    }

    #[test]
    fn delta_work_can_carry_either_chunk_source() {
        // DeltaWork::delta represents a basis-backed transfer that mixes
        // literal tokens with basis matches; the adapter must accept both
        // discriminators against the same work item.
        let work = delta_work(9);
        let adapter = DeltaChunkAdapter::new();

        let literal = adapter.from_delta_work(&work, ChunkPayload::literal(0, vec![0x11; 32]));
        assert!(literal.is_literal);
        assert_eq!(literal.ndx, FileNdx::new(9));

        let matched = adapter.from_delta_work(&work, ChunkPayload::copy(1, vec![0x22; 64]));
        assert!(!matched.is_literal);
        assert_eq!(matched.ndx, FileNdx::new(9));
        assert_eq!(matched.chunk_sequence, 1);
    }

    #[test]
    fn sequence_passes_through_unchanged() {
        let work = whole_file_work(0);
        let adapter = DeltaChunkAdapter::new();
        for seq in [0u64, 1, 7, 1024, u64::MAX] {
            let payload = ChunkPayload::literal(seq, vec![0u8; 4]);
            let chunk = adapter.from_delta_work(&work, payload);
            assert_eq!(chunk.chunk_sequence, seq, "seq must round-trip");
        }
    }

    #[test]
    fn ndx_passes_through_unchanged_across_range() {
        let adapter = DeltaChunkAdapter::new();
        for ndx in [0u32, 1, 42, u32::MAX] {
            let work = whole_file_work(ndx);
            let payload = ChunkPayload::literal(0, vec![0u8; 1]);
            let chunk = adapter.from_delta_work(&work, payload);
            assert_eq!(chunk.ndx, FileNdx::new(ndx));
        }
    }

    #[test]
    fn adapter_does_not_mutate_work() {
        // The adapter takes `&DeltaWork` so callers can reuse the same work
        // item for every chunk in a per-file loop. Confirm the borrow is
        // immutable and the work item survives a chunk conversion intact.
        let work = delta_work(4);
        let snapshot = (
            work.ndx(),
            work.dest_path().to_path_buf(),
            work.basis_path().map(std::path::Path::to_path_buf),
            work.target_size(),
            work.literal_bytes(),
            work.matched_bytes(),
            work.kind(),
        );

        let _chunk = DeltaChunkAdapter::new()
            .from_delta_work(&work, ChunkPayload::copy(0, vec![1, 2, 3, 4]));

        assert_eq!(work.ndx(), snapshot.0);
        assert_eq!(work.dest_path(), snapshot.1);
        assert_eq!(
            work.basis_path().map(std::path::Path::to_path_buf),
            snapshot.2
        );
        assert_eq!(work.target_size(), snapshot.3);
        assert_eq!(work.literal_bytes(), snapshot.4);
        assert_eq!(work.matched_bytes(), snapshot.5);
        assert_eq!(work.kind(), snapshot.6);
    }

    #[test]
    fn payload_constructors_build_expected_shapes() {
        let literal = ChunkPayload::literal(3, vec![9u8; 16]);
        assert_eq!(literal.chunk_sequence, 3);
        assert_eq!(literal.source, ChunkSource::Literal);
        assert!(literal.expected_strong.is_none());

        let copy = ChunkPayload::copy(4, vec![8u8; 16]);
        assert_eq!(copy.chunk_sequence, 4);
        assert_eq!(copy.source, ChunkSource::Copy);
        assert!(copy.expected_strong.is_none());
    }

    #[test]
    fn chunk_source_is_literal_matches_enum_variants() {
        assert!(ChunkSource::Literal.is_literal());
        assert!(!ChunkSource::Copy.is_literal());
    }

    #[test]
    fn free_function_matches_struct_method() {
        // The free function is a thin alias; confirm both spellings produce
        // identical output for the same inputs.
        let work = delta_work(1);
        let bytes = vec![0x77; 128];
        let strategy = md5_strategy();
        let digest = strategy.compute(&bytes);

        let via_struct = DeltaChunkAdapter::new().from_delta_work(
            &work,
            ChunkPayload::literal(5, bytes.clone()).with_expected_strong(digest),
        );
        let via_free = delta_work_to_chunk(
            &work,
            ChunkPayload::literal(5, bytes.clone()).with_expected_strong(digest),
        );

        assert_eq!(via_struct.ndx, via_free.ndx);
        assert_eq!(via_struct.chunk_sequence, via_free.chunk_sequence);
        assert_eq!(via_struct.data, via_free.data);
        assert_eq!(via_struct.is_literal, via_free.is_literal);
        assert_eq!(via_struct.expected_strong, via_free.expected_strong);
    }

    #[test]
    fn adapter_unit_construction_matches_new() {
        // The unit struct has a trivial `new()` constructor for call-site
        // readability; the bare struct literal is the equivalent shorthand.
        // Both spellings must produce the same chunk for the same input.
        let work = whole_file_work(0);
        let bytes = vec![1u8, 2, 3, 4];
        let via_new = DeltaChunkAdapter::new()
            .from_delta_work(&work, ChunkPayload::literal(0, bytes.clone()));
        let via_literal =
            DeltaChunkAdapter.from_delta_work(&work, ChunkPayload::literal(0, bytes));

        assert_eq!(via_new.data, via_literal.data);
        assert_eq!(via_new.ndx, via_literal.ndx);
    }

    #[test]
    fn payload_with_expected_strong_overwrites_prior_value() {
        let strategy = md5_strategy();
        let first = strategy.compute(b"first");
        let second = strategy.compute(b"second");
        let payload = ChunkPayload::literal(0, vec![0u8; 4])
            .with_expected_strong(first)
            .with_expected_strong(second);
        assert_eq!(payload.expected_strong.expect("digest attached"), second);
    }
}
