//! Wire-byte parity regression tests for the opt-in parallel delta scan
//! ([`DeltaGenerator::generate_chunked`]).
//!
//! # Why this file exists
//!
//! The parallel scan splits the source into disjoint ranges and scans each on
//! its own worker with the consumed-bitset prune disabled. That is *not*
//! wire-transparent in general: a matched basis block that straddles a range
//! boundary degrades to literals, and a duplicate-content basis resolves each
//! source window to a different sibling than the pruned sequential scan. The
//! production wiring therefore engages the parallel path only behind a
//! default-off flag and only when the basis is duplicate-free
//! ([`DeltaSignatureIndex::has_duplicate_blocks`] is `false`).
//!
//! These tests pin the two halves of that contract:
//!
//! 1. **Eligible inputs are byte-identical.** For a duplicate-free basis whose
//!    source is an in-place edit at every range-boundary offset - the exact
//!    layout where a naive split would straddle a match - the chunked wire
//!    bytes must equal the sequential wire bytes for chunk counts 2, 4, and 8.
//!    This is the guarantee the opt-in relies on. If the adjacent-literal
//!    coalescing or the boundary handling regresses, this equality breaks.
//! 2. **Ineligible inputs are documented as divergent.** For a
//!    duplicate-heavy basis the chunked and sequential wire bytes must
//!    *differ*, pinning the boundary the duplicate-free gate exists to avoid:
//!    a future change cannot silently make the parallel path look transparent
//!    on duplicate content and thereby hide the divergence the gate guards
//!    against.
//!
//! The wire serialization mirrors `transfer::generator::script_to_wire_delta`
//! feeding `protocol::wire::write_token_stream`, exactly as
//! `zsync_wire_parity.rs` does, so the pinned bytes are the ones that travel
//! over an rsync stream.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use protocol::wire::{DeltaOp, write_token_stream};
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Deterministic LCG byte stream (same generator as `zsync_wire_parity.rs`).
fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state: u64 = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.push((state >> 33) as u8);
    }
    out
}

/// Builds a [`DeltaSignatureIndex`] with a forced block length and MD4 strong
/// checksum, matching `zsync_wire_parity.rs`.
fn build_index(basis: &[u8], block_len: u32) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(block_len).expect("block_len > 0")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("checksum length"),
    );
    let layout = calculate_signature_layout(params).expect("signature layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Mirrors `transfer::generator::script_to_wire_delta`: expands a seq-match
/// coalesced fat `Copy` into one per-block token so the wire byte stream is
/// independent of internal coalescing.
fn script_to_ops(script: &DeltaScript, block_len: usize) -> Vec<DeltaOp> {
    let mut ops = Vec::with_capacity(script.tokens().len());
    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => ops.push(DeltaOp::Literal(data.clone())),
            DeltaToken::Copy { index, len } => {
                if block_len > 0 && *len > block_len && *len % block_len == 0 {
                    let run = *len / block_len;
                    for k in 0..run {
                        ops.push(DeltaOp::Copy {
                            block_index: u32::try_from(*index + k as u64)
                                .expect("block index fits in u32"),
                            length: u32::try_from(block_len).expect("block_len fits in u32"),
                        });
                    }
                } else {
                    ops.push(DeltaOp::Copy {
                        block_index: u32::try_from(*index).expect("block index fits in u32"),
                        length: u32::try_from(*len).expect("copy length fits in u32"),
                    });
                }
            }
        }
    }
    ops
}

/// Serializes a [`DeltaScript`] into the exact token-frame byte stream that
/// travels over an rsync protocol stream.
fn script_to_wire_bytes(script: &DeltaScript, block_len: usize) -> Vec<u8> {
    let ops = script_to_ops(script, block_len);
    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).expect("token stream serialises");
    buf
}

/// Reconstructs the source from `basis` by applying `script`.
fn reconstruct(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript) -> Vec<u8> {
    let mut cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::new();
    apply_delta(&mut cursor, &mut output, index, script).expect("apply");
    output
}

/// Block length used by the parity fixtures. Small enough to keep the fixtures
/// modest, and chosen together with `DUP_FREE_LEN` so that every chunk
/// boundary lands strictly inside a block (a boundary-straddling layout).
const BLOCK_LEN: u32 = 700;

/// Source length for the duplicate-free fixture.
///
/// Divisible by 8 (so `n/8`, `n/4`, `n/2` are exact and the chunk-4 and
/// chunk-2 boundaries are a subset of the chunk-8 boundaries) and by
/// `BLOCK_LEN` (no trailing partial block), while `n/8` is **not** a multiple
/// of `BLOCK_LEN` - so each chunk boundary straddles a block. Large enough
/// that `generate_chunked` actually splits into 8 ranges (the internal floor
/// is `max(1 MiB, block_len * 64)` per range).
const DUP_FREE_LEN: usize = 8_390_200;

/// The parallel scan must be byte-identical to the sequential scan for a
/// duplicate-free basis whose source is an in-place edit at every chunk
/// boundary - the worst case for boundary handling.
///
/// Each single-byte edit sits exactly on a chunk boundary, so the block that
/// straddles that boundary is non-matching in *both* scans (its strong
/// checksum changed) and the only thing that must line up is the literal-token
/// framing across the range join, which the concatenation-time coalescing
/// restores. Every other block matches at its aligned offset exactly once, so
/// the disabled prune is a no-op. The result: identical `Copy` index sequence,
/// identical literal runs, identical wire bytes.
#[test]
fn parallel_delta_dup_free_is_wire_identical() {
    let basis = lcg_bytes(0x9A7A_11E1_0DE1_2025, DUP_FREE_LEN);
    let index = build_index(&basis, BLOCK_LEN);
    assert!(
        !index.has_duplicate_blocks(),
        "random basis must be duplicate-free so the parallel path is eligible"
    );

    // Edit one byte at every chunk-8 boundary. Because DUP_FREE_LEN is
    // divisible by 8, the chunk-4 and chunk-2 boundaries are a subset of these
    // offsets, so a single loop covers every boundary for chunks in {2,4,8}.
    let base8 = DUP_FREE_LEN / 8;
    let mut source = basis.clone();
    for k in 1..8 {
        source[k * base8] ^= 0xff;
    }

    let generator = DeltaGenerator::new();
    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    let seq_wire = script_to_wire_bytes(&sequential, index.block_length());

    // Sanity: the edits must actually leave most of the file as copies, or the
    // test would be trivially satisfied by an all-literal stream.
    assert!(
        sequential.copy_bytes() > (DUP_FREE_LEN as u64) * 9 / 10,
        "sparse edits must leave the bulk of the file matched (copy_bytes={})",
        sequential.copy_bytes()
    );

    for &chunks in &[2usize, 4, 8] {
        let chunked = generator
            .generate_chunked(&source, &index, chunks)
            .expect("chunked");
        let chunked_wire = script_to_wire_bytes(&chunked, index.block_length());

        assert_eq!(
            chunked_wire, seq_wire,
            "chunked wire bytes must equal sequential for chunks={chunks} on a \
             duplicate-free basis with boundary-aligned edits"
        );
        assert_eq!(
            reconstruct(&basis, &index, &chunked),
            source,
            "chunked reconstruction must equal the source for chunks={chunks}"
        );
    }
}

/// Source length for the duplicate-heavy fixture: large enough to split into
/// several ranges (> 2x the 1 MiB per-range floor).
const DUP_HEAVY_LEN: usize = 4 * 1024 * 1024;

/// The parallel scan must **differ** from the sequential scan on a
/// duplicate-heavy basis, pinning the divergence the duplicate-free gate
/// exists to avoid.
///
/// With three distinct block contents repeated `A B C A B C ...`, the pruned
/// sequential scan matches each duplicate sibling once and emits an ascending,
/// position-accurate `Copy` index sequence. The prune-off parallel scan, when
/// a range starts mid-file, resolves the first occurrence of each content to
/// the lowest-indexed sibling and walks the successor chain from there, so its
/// `Copy` indices no longer track the true source offsets. The wire bytes must
/// diverge - if they ever stop diverging, the gate could be silently dropped.
#[test]
fn parallel_delta_dup_heavy_diverges() {
    let block_a = lcg_bytes(0x0DDB_A5E5_CAB7_E5A0, BLOCK_LEN as usize);
    let block_b = lcg_bytes(0x0DDB_A5E5_CAB7_E5B0, BLOCK_LEN as usize);
    let block_c = lcg_bytes(0x0DDB_A5E5_CAB7_E5C0, BLOCK_LEN as usize);

    let mut basis = Vec::with_capacity(DUP_HEAVY_LEN + 3 * BLOCK_LEN as usize);
    while basis.len() < DUP_HEAVY_LEN {
        basis.extend_from_slice(&block_a);
        basis.extend_from_slice(&block_b);
        basis.extend_from_slice(&block_c);
    }
    let index = build_index(&basis, BLOCK_LEN);
    assert!(
        index.has_duplicate_blocks(),
        "A/B/C repetition must be flagged as duplicate-heavy"
    );

    let source = basis.clone();
    let generator = DeltaGenerator::new();

    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    let seq_wire = script_to_wire_bytes(&sequential, index.block_length());

    let chunked = generator
        .generate_chunked(&source, &index, 4)
        .expect("chunked");
    let chunked_wire = script_to_wire_bytes(&chunked, index.block_length());

    // Both still reconstruct the source: divergence is in the token shape, not
    // correctness.
    assert_eq!(reconstruct(&basis, &index, &chunked), source);
    assert_eq!(reconstruct(&basis, &index, &sequential), source);

    assert_ne!(
        chunked_wire, seq_wire,
        "duplicate-heavy basis must produce divergent chunked vs sequential wire \
         bytes; this is exactly why the wiring gates the parallel path on a \
         duplicate-free basis"
    );
}
