//! Pins bail-out 2 of the parallel delta scan: `--inplace`
//! (`updating_basis_file`) must route [`DeltaGenerator::generate_chunked`]
//! back to the sequential scan.
//!
//! # Why this route exists
//!
//! Upstream's in-place guard (upstream: match.c:232-239, 3.5.0) unlinks any
//! hash-chain candidate whose basis offset precedes the running source
//! `offset` (unless `SUMFLG_SAME_OFFSET`): under `--inplace` the receiver
//! overwrites the basis as it reconstructs, so a backward-referencing `Copy`
//! would read bytes an earlier write already clobbered. The guard compares
//! against the GLOBAL source cursor. A parallel stripe knows only a
//! stripe-local cursor, so a mid-file stripe would trust backward matches the
//! sequential scan suppresses - out-of-order copies that are unsafe to apply
//! in place. `generate_chunked` therefore falls back to the sequential scan
//! whenever `updating_basis_file` is set (`crates/matching/src/generator.rs`,
//! the `self.updating_basis_file` bail-out).
//!
//! Two pins:
//!
//! 1. **Correctness** - on a fixture whose second half matches the basis's
//!    FIRST half (every match is backward at global scale, forward at
//!    stripe-local scale), the guarded chunked scan equals the guarded
//!    sequential scan token-for-token, suppresses every backward copy, and
//!    reconstructs the source exactly. A discrimination control shows the
//!    striped scan WOULD emit those backward copies were the guard not
//!    routing it away.
//! 2. **Routing** - the guarded chunked scan takes the pruned sequential
//!    path, observed through the shared consumed-bitset seam: the sequential
//!    scan marks matched blocks consumed, while the striped path clears the
//!    bitset and never writes it (pruning is off per stripe).

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Deterministic LCG byte stream (same generator as
/// `parallel_delta_wire_parity.rs`).
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
/// checksum, matching `parallel_delta_wire_parity.rs`.
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

/// Reconstructs the source from `basis` by applying `script`.
fn reconstruct(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript) -> Vec<u8> {
    let mut cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::new();
    apply_delta(&mut cursor, &mut output, index, script).expect("apply");
    output
}

/// Asserts the in-place invariant on a token stream: every `Copy`'s basis
/// offset is at or ahead of the reconstruction cursor when it is emitted
/// (upstream: match.c:232-239 admits `offset >= our offset` or same-offset).
fn assert_monotonic_basis_offsets(script: &DeltaScript, block_len: usize) {
    let mut cursor = 0u64;
    for token in script.tokens() {
        if let DeltaToken::Copy { index, len } = token {
            let basis_offset = index * block_len as u64;
            assert!(
                basis_offset >= cursor,
                "backward copy under --inplace: basis offset {basis_offset} < \
                 write cursor {cursor} (block index {index}, len {len})"
            );
        }
        cursor += token.byte_len() as u64;
    }
}

/// Block length shared with the sibling parity fixtures.
const BLOCK_LEN: u32 = 700;

/// Fixture length: 6000 full blocks (~4 MiB), no short final block, and large
/// enough that `generate_chunked`'s internal floor (`max(1 MiB, 64 blocks)`
/// per stripe) still yields 4 stripes - so bail-out 3 ("too small to split")
/// cannot mask bail-out 2.
const N: usize = BLOCK_LEN as usize * 6000;

/// Block-aligned midpoint of the fixture.
const HALF: usize = N / 2;

/// Chunk count requested from `generate_chunked`; the fixture admits it.
const CHUNKS: usize = 4;

/// Source whose entire second half equals the basis's FIRST half: every match
/// is backward-referencing at the global cursor, but forward at a mid-file
/// stripe's local cursor - the exact shape striping would mis-admit.
fn backward_fixture(basis: &[u8]) -> Vec<u8> {
    let mut source = lcg_bytes(0x1260_BA11_0FF5_2026, HALF);
    source.extend_from_slice(&basis[..HALF]);
    source
}

/// Pin (a), correctness: `--inplace` + parallel scan produces the guarded
/// sequential scan's exact token stream on the backward fixture - every
/// backward match suppressed to literals, reconstruction byte-exact.
#[test]
fn inplace_chunked_matches_guarded_sequential_on_backward_fixture() {
    let basis = lcg_bytes(0x1260_1259_1258_2026, N);
    let index = build_index(&basis, BLOCK_LEN);
    assert!(
        !index.has_duplicate_blocks(),
        "random basis must be duplicate-free so only bail-out 2 can reroute"
    );
    let source = backward_fixture(&basis);

    let guarded = DeltaGenerator::new().with_updating_basis_file(true);
    let sequential = guarded
        .generate(Cursor::new(source.clone()), &index)
        .expect("guarded sequential");
    // Every candidate the second half can match lies strictly behind the
    // global cursor, so the guarded scan must suppress them all.
    assert_eq!(
        sequential.copy_bytes(),
        0,
        "the backward fixture must leave the guarded sequential scan all-literal"
    );

    let chunked = guarded
        .generate_chunked(&source, &index, CHUNKS)
        .expect("guarded chunked");
    assert_monotonic_basis_offsets(&chunked, index.block_length());
    assert_eq!(
        chunked.tokens(),
        sequential.tokens(),
        "--inplace must route generate_chunked onto the guarded sequential \
         scan's exact token stream"
    );
    assert_eq!(
        reconstruct(&basis, &index, &chunked),
        source,
        "guarded chunked reconstruction must equal the source"
    );

    // Discrimination control: without the guard, the striped scan DOES admit
    // the backward matches (a mid-file stripe's local cursor starts at zero),
    // so the fixture genuinely separates the two routes - the parity above is
    // the bail-out working, not the fixture failing to discriminate.
    let unguarded_chunked = DeltaGenerator::new()
        .generate_chunked(&source, &index, CHUNKS)
        .expect("unguarded chunked");
    assert!(
        unguarded_chunked.copy_bytes() >= (HALF as u64) * 9 / 10,
        "the unguarded striped scan must match the bulk of the backward half \
         (copy_bytes={})",
        unguarded_chunked.copy_bytes()
    );
}

/// Pin (b), routing: `--inplace` + parallel scan takes the pruned SEQUENTIAL
/// path, observed through the consumed-bitset seam. The sequential scan marks
/// every matched block consumed; the striped path clears the bitset and never
/// writes it (per-stripe pruning is off), so a forced striping under
/// `--inplace` leaves the bitset empty and turns this test red.
#[test]
fn inplace_chunked_takes_sequential_path() {
    let basis = lcg_bytes(0x1260_C0DE_5EA1_2026, N);
    let index = build_index(&basis, BLOCK_LEN);
    assert!(
        !index.has_duplicate_blocks(),
        "fixture must be duplicate-free"
    );
    // Source == basis: every block matches at its own offset, which the
    // in-place guard admits (`>=` is not strict), so the sequential scan
    // marks every block consumed.
    let source = basis.clone();
    let block_count = index.block_count();

    let guarded = DeltaGenerator::new().with_updating_basis_file(true);
    let chunked = guarded
        .generate_chunked(&source, &index, CHUNKS)
        .expect("guarded chunked");
    assert_eq!(reconstruct(&basis, &index, &chunked), source);

    let consumed_after_guarded = (0..block_count as u32)
        .filter(|&i| index.is_consumed(i))
        .count();
    assert!(
        consumed_after_guarded > block_count * 9 / 10,
        "--inplace must take the pruned sequential path, which marks matched \
         blocks consumed (consumed {consumed_after_guarded} of {block_count})"
    );

    // Seam control: the striped path (guard off) resets the bitset and never
    // marks it, so the observable cleanly discriminates the two routes.
    let striped = DeltaGenerator::new()
        .generate_chunked(&source, &index, CHUNKS)
        .expect("striped");
    assert_eq!(reconstruct(&basis, &index, &striped), source);
    let consumed_after_striped = (0..block_count as u32)
        .filter(|&i| index.is_consumed(i))
        .count();
    assert_eq!(
        consumed_after_striped, 0,
        "the striped scan clears the consumed bitset and scans prune-off; a \
         non-empty bitset here would blind the routing pin above"
    );
}
