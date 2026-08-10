//! REFLINK-4 regression tests.
//!
//! Drive `DeltaApplicator::apply_delta_stream` with a synthetic COPY token that
//! satisfies the partial-reflink fast path's gating predicate (range size,
//! alignment, no checksum verifier, no sparse) and assert the output bytes
//! match the basis. The point of these tests is byte-identical output across
//! every platform regardless of whether the underlying filesystem actually
//! satisfies `FICLONERANGE`:
//!
//! - On Linux btrfs / XFS / bcachefs with a CoW backing volume, the wrapper
//!   returns `Ok(true)` and the destination shares extents with the basis.
//! - On Linux ext4 / tmpfs (and macOS / Windows), the wrapper returns
//!   `Ok(false)` and the receiver falls through to `copy_file_range(2)` or
//!   the read+write path. The output content is identical either way; the
//!   tests would fail only if the FICLONERANGE wiring corrupted the data
//!   path on the failure branch.

#![deny(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::num::{NonZeroU8, NonZeroU32};

use checksums::RollingDigest;
use engine::signature::{FileSignature, SignatureBlock};
use signature::SignatureLayout;
use tempfile::tempdir;
use transfer::delta_apply::{
    BasisWriterKind, ChecksumVerifier, DeltaApplicator, DeltaApplyConfig, TokenReader,
    apply_delta_stream,
};

const BLOCK_LEN: u32 = 4096;
const BLOCK_COUNT: u64 = 16;
const FILE_LEN: u64 = BLOCK_LEN as u64 * BLOCK_COUNT;

/// Builds a synthetic basis payload whose bytes are deterministic so we can
/// assert against them after delta application.
fn basis_payload() -> Vec<u8> {
    (0..FILE_LEN as usize)
        .map(|i| ((i * 31 + 7) % 251) as u8)
        .collect()
}

/// Constructs a `FileSignature` whose layout matches the synthetic basis: a
/// 4 KiB block size (matches the FICLONERANGE alignment requirement) with no
/// remainder. We do not need real rolling / strong sums here because the
/// applicator does not consult them for COPY tokens - the block index alone
/// identifies the source range.
fn make_signature() -> FileSignature {
    let layout = SignatureLayout::from_raw_parts(
        NonZeroU32::new(BLOCK_LEN).expect("block length nonzero"),
        0,
        BLOCK_COUNT,
        NonZeroU8::new(16).expect("strong sum length nonzero"),
    );
    let blocks: Vec<SignatureBlock> = (0..BLOCK_COUNT)
        .map(|idx| SignatureBlock::from_raw_parts(idx, RollingDigest::ZERO, &[0u8; 16]))
        .collect();
    FileSignature::from_raw_parts(layout, blocks, FILE_LEN)
}

/// Encodes a `COPY(block_idx)` token in the wire format used by
/// `apply_token`: 4-byte little-endian `i32`, value `-(block_idx + 1)`.
fn copy_token(block_idx: u64) -> [u8; 4] {
    let token = -(i32::try_from(block_idx).expect("block index fits in i32") + 1);
    token.to_le_bytes()
}

/// End-of-stream sentinel: a single zero token.
const END_TOKEN: [u8; 4] = [0, 0, 0, 0];

/// Sets up a temp basis file written with `payload`, returns its path along
/// with an empty destination file opened for read/write.
fn setup(payload: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, File) {
    let dir = tempdir().expect("tempdir");
    let basis_path = dir.path().join("basis.bin");
    {
        let mut basis = File::create(&basis_path).expect("create basis");
        basis.write_all(payload).expect("write basis");
        basis.sync_all().ok();
    }
    let dest_path = dir.path().join("dest.bin");
    let dest = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&dest_path)
        .expect("create dest");
    (dir, basis_path, dest)
}

/// Reads `dest` back from offset zero into a Vec.
fn read_back(mut dest: File) -> Vec<u8> {
    dest.seek(SeekFrom::Start(0)).expect("seek 0");
    let mut buf = Vec::new();
    dest.read_to_end(&mut buf).expect("read dest");
    buf
}

/// Single 4 KiB-aligned COPY token at offset 0 reconstructs the first block
/// byte-for-byte. This exercises the FICLONERANGE-eligible path: aligned
/// offsets, len >= CLONE_FILE_RANGE_MIN_BYTES, no checksum, no sparse.
#[test]
fn aligned_copy_token_at_offset_zero_matches_basis_block() {
    let payload = basis_payload();
    let (_dir, basis_path, dest) = setup(&payload);
    let signature = make_signature();
    let config = DeltaApplyConfig {
        sparse: false,
        writer_kind: BasisWriterKind::Standard,
        cow_policy: fast_io::CowPolicy::Auto,
    };
    let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
    let mut applicator = DeltaApplicator::new(
        dest,
        &config,
        verifier,
        Some(&signature),
        Some(basis_path.as_path()),
    )
    .expect("construct applicator");

    let mut wire = Vec::new();
    wire.extend_from_slice(&copy_token(0));
    wire.extend_from_slice(&END_TOKEN);
    let mut reader = Cursor::new(wire);
    let mut token_reader = TokenReader::new(None).expect("plain token reader");

    apply_delta_stream(&mut reader, &mut applicator, &mut token_reader)
        .expect("apply delta stream");
    // Drop the applicator (and its owned File) before reopening so any
    // buffered state is released. apply_delta_stream does not call finish().
    drop(applicator);

    // Re-open the destination to read back what was written. The applicator
    // owns the File handle, so we open the underlying path independently.
    let dest_path = basis_path.with_file_name("dest.bin");
    let dest_back = File::open(&dest_path).expect("reopen dest");
    let out = read_back(dest_back);
    assert_eq!(
        out,
        payload[..BLOCK_LEN as usize].to_vec(),
        "first-block COPY token must reconstruct basis bytes exactly"
    );
}

/// Multiple sequential aligned COPY tokens reconstruct the basis without
/// gaps. The FICLONERANGE path either succeeds for each token (cache stays
/// `Supported`) or the cache transitions to `Declined` after the first miss
/// and subsequent tokens take the `copy_file_range` fallback. Either way the
/// output is byte-identical.
#[test]
fn sequential_aligned_copy_tokens_reconstruct_full_basis() {
    let payload = basis_payload();
    let (_dir, basis_path, dest) = setup(&payload);
    let signature = make_signature();
    let config = DeltaApplyConfig {
        sparse: false,
        writer_kind: BasisWriterKind::Standard,
        cow_policy: fast_io::CowPolicy::Auto,
    };
    let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
    let mut applicator = DeltaApplicator::new(
        dest,
        &config,
        verifier,
        Some(&signature),
        Some(basis_path.as_path()),
    )
    .expect("construct applicator");

    let mut wire = Vec::new();
    for idx in 0..BLOCK_COUNT {
        wire.extend_from_slice(&copy_token(idx));
    }
    wire.extend_from_slice(&END_TOKEN);
    let mut reader = Cursor::new(wire);
    let mut token_reader = TokenReader::new(None).expect("plain token reader");

    apply_delta_stream(&mut reader, &mut applicator, &mut token_reader)
        .expect("apply delta stream");
    drop(applicator);

    let dest_path = basis_path.with_file_name("dest.bin");
    let dest_back = File::open(&dest_path).expect("reopen dest");
    let out = read_back(dest_back);
    assert_eq!(
        out, payload,
        "sequential COPY tokens must reproduce the basis"
    );
}

/// COPY at a non-aligned destination offset (because a tiny literal precedes
/// it) must NOT use FICLONERANGE - the alignment guard declines and the
/// fallback path still reconstructs the bytes correctly.
#[test]
fn literal_then_copy_falls_through_alignment_guard() {
    let payload = basis_payload();
    let (_dir, basis_path, dest) = setup(&payload);
    let signature = make_signature();
    let config = DeltaApplyConfig {
        sparse: false,
        writer_kind: BasisWriterKind::Standard,
        cow_policy: fast_io::CowPolicy::Auto,
    };
    let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
    let mut applicator = DeltaApplicator::new(
        dest,
        &config,
        verifier,
        Some(&signature),
        Some(basis_path.as_path()),
    )
    .expect("construct applicator");

    let literal = b"hi!"; // 3-byte literal -> dest offset becomes 3, unaligned
    let literal_len = literal.len() as i32;
    let mut wire = Vec::new();
    wire.extend_from_slice(&literal_len.to_le_bytes());
    wire.extend_from_slice(literal);
    wire.extend_from_slice(&copy_token(0));
    wire.extend_from_slice(&END_TOKEN);
    let mut reader = Cursor::new(wire);
    let mut token_reader = TokenReader::new(None).expect("plain token reader");

    apply_delta_stream(&mut reader, &mut applicator, &mut token_reader)
        .expect("apply delta stream");
    drop(applicator);

    let dest_path = basis_path.with_file_name("dest.bin");
    let dest_back = File::open(&dest_path).expect("reopen dest");
    let out = read_back(dest_back);

    let mut expected = Vec::new();
    expected.extend_from_slice(literal);
    expected.extend_from_slice(&payload[..BLOCK_LEN as usize]);
    assert_eq!(
        out, expected,
        "unaligned-dest COPY must still reconstruct via the fallback path"
    );
}
