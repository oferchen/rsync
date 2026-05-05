//! Adversarial regression fixture: shifted-insertion source vs unchanged basis.
//!
//! The zsync-inspired matching design (`docs/design/zsync-inspired-matching.md`)
//! lists shifted insertion as a primary regression target for any future
//! probabilistic prefilter (bithash, compact-keys, seq-match). This fixture
//! exercises the same property at the public API level so that any
//! optimisation landed under the index/generator boundary cannot silently
//! lose a match shifted by `N` bytes against an aligned basis.
//!
//! # Construction
//!
//! For each entry in the `(block_size, N, M, algo)` matrix the fixture builds
//! a deterministic basis of seeded bytes, then constructs a source by
//! inserting `N` bytes at offset `M`. The source is therefore exactly
//! `basis.len() + N` bytes long, and the bytes after offset `M + N` of the
//! source are byte-identical to the bytes after offset `M` of the basis.
//!
//! # What is verified
//!
//! 1. The script's total byte count equals `source.len()`.
//! 2. Apply on the basis reproduces the source byte-for-byte (functional
//!    correctness gate; this is the wire-compat invariant).
//! 3. Literal bytes account for at least `N` (the inserted region is never
//!    matchable by the basis index by construction).
//! 4. Aligned inserts (`M % block_size == 0` and `N % block_size == 0`)
//!    produce a `Copy + Literal + Copy` token sequence that is byte-identical
//!    to the basis copy with a shifted offset boundary.
//! 5. Unaligned inserts lose at most one mid-stride block to boundary drift,
//!    so matched bytes are at least `basis.len() - block_size`.
//!
//! # Strong-checksum coverage
//!
//! The matrix runs MD5 (the protocol >= 30 default) and XXH3-64 (the modern
//! negotiated alternative). MD4 is intentionally excluded from the matrix
//! because the strategy selector at
//! `crates/checksums/src/strong/strategy/selector.rs` only emits it for
//! protocol < 30; a separate single-shot test pins the MD4 path explicitly.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/match.c:140-345` -
//!   `hash_search()` and the `want_i` adjacent-block hint that aligned
//!   shifted inserts must continue to satisfy.

use checksums::strong::Md5Seed;
use matching::{DeltaScript, DeltaSignatureIndex, DeltaToken, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Basis-file lengths sized so that the largest insert (1024 bytes at offset
/// `2 * block_size + 1`) still leaves several full blocks past the insert.
const BASIS_BLOCKS: usize = 8;

/// Block sizes covered by the matrix. 700 is upstream's default for small
/// files, 1024 hits a clean power-of-two boundary that the rolling-window
/// fast paths special-case, and 4096 stresses the buffer-refill loop after
/// each match.
const BLOCK_SIZES: &[u32] = &[700, 1024, 4096];

/// Insertion lengths: 1 byte (forces a per-byte rolling re-sync), 7 bytes
/// (sub-word), 31 bytes (sub-block but past most SIMD lane widths), and a
/// full 1024-byte run that aligns with the 1024 block-size case.
const INSERT_LENS: &[usize] = &[1, 7, 31, 1024];

/// Builds a deterministic basis. `(i * 17 + 31) as u8` gives a high-entropy,
/// reproducible byte stream that does not collapse onto a small alphabet.
fn make_basis(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(17).wrapping_add(31)) as u8)
        .collect()
}

/// Inserts `N` filler bytes at `offset` inside `basis`, returning the new
/// source. The filler uses a different seed so it cannot accidentally match
/// any window in the basis.
fn make_source_with_insertion(basis: &[u8], offset: usize, n: usize) -> Vec<u8> {
    assert!(offset <= basis.len());
    let mut source = Vec::with_capacity(basis.len() + n);
    source.extend_from_slice(&basis[..offset]);
    source.extend((0..n).map(|i| (i.wrapping_mul(101).wrapping_add(7) ^ 0xA5) as u8));
    source.extend_from_slice(&basis[offset..]);
    source
}

/// Builds a [`DeltaSignatureIndex`] for `basis` with a forced block length
/// and the requested strong-checksum strategy. Returns `None` if the basis
/// is too small to contain a full block.
fn build_index(
    basis: &[u8],
    block_len: u32,
    algorithm: SignatureAlgorithm,
) -> Option<DeltaSignatureIndex> {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(block_len).expect("block_len non-zero")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("checksum length non-zero"),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(basis, layout, algorithm).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, algorithm)
}

/// Drives `generate_delta` over `source` against an index built from `basis`.
fn run_pipeline(
    basis: &[u8],
    source: &[u8],
    block_len: u32,
    algorithm: SignatureAlgorithm,
) -> (DeltaSignatureIndex, DeltaScript) {
    let index = build_index(basis, block_len, algorithm).expect("index built");
    let script = generate_delta(Cursor::new(source), &index).expect("delta generated");
    (index, script)
}

/// Reconstructs the source by applying `script` to `basis` and asserts
/// byte-for-byte equality. This is the wire-compat correctness gate: any
/// optimisation that breaks reconstruction is incorrect by construction.
fn assert_round_trip(
    basis: &[u8],
    source: &[u8],
    index: &DeltaSignatureIndex,
    script: &DeltaScript,
) {
    let mut basis_cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::with_capacity(source.len());
    matching::apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply");
    assert_eq!(output, source, "round-trip reconstruction must match source");
}

/// Returns the strong-checksum strategies the matrix iterates. MD5 with
/// `Md5Seed::none()` is the post-protocol-30 default; XXH3 mirrors the
/// negotiated modern alternative. Both go through the same
/// `find_match_*` strong-checksum gate inside `DeltaSignatureIndex`.
fn matrix_algorithms() -> [SignatureAlgorithm; 2] {
    [
        SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        },
        SignatureAlgorithm::Xxh3 { seed: 0 },
    ]
}

/// Aligned-insert run: when both `M` and `N` are integral multiples of
/// `block_size`, the rolling-window aligns at every block boundary and the
/// generator must produce a `Copy(prefix) + Literal(N) + Copy(suffix)` token
/// shape, with matched bytes equal to `basis.len()` exactly.
fn assert_aligned_insert_shape(
    block_len: u32,
    insert_offset_blocks: usize,
    insert_blocks: usize,
    algorithm: SignatureAlgorithm,
) {
    let block_size = block_len as usize;
    let basis_len = BASIS_BLOCKS * block_size;
    let basis = make_basis(basis_len);
    let m = insert_offset_blocks * block_size;
    let n = insert_blocks * block_size;
    let source = make_source_with_insertion(&basis, m, n);

    let (index, script) = run_pipeline(&basis, &source, block_len, algorithm);
    assert_round_trip(&basis, &source, &index, &script);

    assert_eq!(script.total_bytes(), source.len() as u64);
    assert_eq!(
        script.literal_bytes(),
        n as u64,
        "aligned insert: literals account for exactly the inserted bytes \
         (block_len={block_len}, M_blocks={insert_offset_blocks}, N_blocks={insert_blocks}, \
         algo={algorithm:?})"
    );
    assert_eq!(
        script.copy_bytes(),
        basis_len as u64,
        "aligned insert: every basis block is matched"
    );

    let copy_indices: Vec<u64> = script
        .tokens()
        .iter()
        .filter_map(|tok| match tok {
            DeltaToken::Copy { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let mut expected: Vec<u64> = (0..insert_offset_blocks as u64).collect();
    expected.extend(insert_offset_blocks as u64..BASIS_BLOCKS as u64);
    assert_eq!(
        copy_indices, expected,
        "aligned insert COPY token indices must form a contiguous prefix \
         then a contiguous suffix shifted by N bytes (block_len={block_len}, \
         algo={algorithm:?})"
    );
}

/// Unaligned-insert run: at most one mid-stride block can be lost to the
/// boundary because the rolling window must re-sync past the inserted
/// bytes before the next basis block aligns again.
fn assert_unaligned_insert_bounds(
    block_len: u32,
    m: usize,
    n: usize,
    algorithm: SignatureAlgorithm,
) {
    let block_size = block_len as usize;
    let basis_len = BASIS_BLOCKS * block_size;
    let basis = make_basis(basis_len);
    let source = make_source_with_insertion(&basis, m, n);

    let (index, script) = run_pipeline(&basis, &source, block_len, algorithm);
    assert_round_trip(&basis, &source, &index, &script);

    assert_eq!(script.total_bytes(), source.len() as u64);
    assert!(
        script.literal_bytes() >= n as u64,
        "unaligned insert: literal byte count must include the {n} inserted bytes \
         (got literal={}, block_len={block_len}, M={m}, N={n}, algo={algorithm:?})",
        script.literal_bytes()
    );

    let lower_bound = (basis_len as u64).saturating_sub(block_size as u64);
    assert!(
        script.copy_bytes() >= lower_bound,
        "unaligned insert: matched bytes must be at least basis_len - block_size \
         (got copy={}, lower_bound={lower_bound}, block_len={block_len}, \
         M={m}, N={n}, algo={algorithm:?})",
        script.copy_bytes()
    );
    let upper_bound = basis_len as u64;
    assert!(
        script.copy_bytes() <= upper_bound,
        "unaligned insert: matched bytes cannot exceed basis_len \
         (got copy={}, upper_bound={upper_bound})",
        script.copy_bytes()
    );
}

/// Drives every `(block_size, N, M, algorithm)` cell where `N` and `M` both
/// land on aligned block boundaries. These must produce the exact COPY/Literal
/// shape described in the assertion helper.
#[test]
fn aligned_inserts_yield_byte_identical_shifted_copy_sequence() {
    for &block_len in BLOCK_SIZES {
        for algorithm in matrix_algorithms() {
            // Insert one full block at the start (prepend), in the middle, and
            // immediately before the last block. Insert lengths of 1 and 2
            // blocks both exercise the multi-block literal path.
            for &insert_offset_blocks in &[0usize, BASIS_BLOCKS / 2, BASIS_BLOCKS - 1] {
                for &insert_blocks in &[1usize, 2] {
                    assert_aligned_insert_shape(
                        block_len,
                        insert_offset_blocks,
                        insert_blocks,
                        algorithm,
                    );
                }
            }
        }
    }
}

/// Drives the unaligned cells of the matrix. `M` cycles through `0`,
/// `block_size / 2`, `block_size`, and `2 * block_size + 1`; `N` cycles
/// through `INSERT_LENS`. Aligned `(M, N)` pairs are skipped here because
/// they are covered by `aligned_inserts_yield_byte_identical_shifted_copy_sequence`.
#[test]
fn unaligned_inserts_match_within_one_block_of_basis_length() {
    for &block_len in BLOCK_SIZES {
        let block_size = block_len as usize;
        let offsets = [0usize, block_size / 2, block_size, 2 * block_size + 1];
        for algorithm in matrix_algorithms() {
            for &m in &offsets {
                for &n in INSERT_LENS {
                    let aligned = m % block_size == 0 && n % block_size == 0;
                    if aligned {
                        continue;
                    }
                    assert_unaligned_insert_bounds(block_len, m, n, algorithm);
                }
            }
        }
    }
}

/// Prepend corner case: inserting at offset 0 turns the entire basis into a
/// shifted-by-N suffix. With aligned `N` this must yield a single contiguous
/// copy run starting at block 0.
#[test]
fn prepend_aligned_insertion_preserves_full_basis_match() {
    let block_len: u32 = 1024;
    let block_size = block_len as usize;
    let basis = make_basis(BASIS_BLOCKS * block_size);
    let n = block_size;
    let source = make_source_with_insertion(&basis, 0, n);

    let (index, script) = run_pipeline(
        &basis,
        &source,
        block_len,
        SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        },
    );
    assert_round_trip(&basis, &source, &index, &script);

    assert_eq!(script.literal_bytes(), n as u64);
    assert_eq!(script.copy_bytes(), basis.len() as u64);

    // First non-literal token after the prepended block must reference basis
    // block 0; the subsequent COPY tokens must run contiguously to the end.
    let copy_sequence: Vec<u64> = script
        .tokens()
        .iter()
        .filter_map(|tok| match tok {
            DeltaToken::Copy { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let expected: Vec<u64> = (0..BASIS_BLOCKS as u64).collect();
    assert_eq!(copy_sequence, expected);
}

/// Tail-edge corner case: inserting at the byte just before the end of the
/// basis still produces a valid round trip and accounts for the inserted
/// bytes as literals. The final basis block straddles the inserted bytes,
/// so it cannot match in full and its bytes fall out as literals.
#[test]
fn tail_edge_insertion_round_trips() {
    let block_len: u32 = 700;
    let block_size = block_len as usize;
    let basis = make_basis(BASIS_BLOCKS * block_size);
    let m = basis.len() - 1;
    let n = 7;
    let source = make_source_with_insertion(&basis, m, n);

    let (index, script) = run_pipeline(&basis, &source, block_len, SignatureAlgorithm::Xxh3 { seed: 0 });
    assert_round_trip(&basis, &source, &index, &script);

    assert_eq!(script.total_bytes(), source.len() as u64);
    assert!(script.literal_bytes() >= n as u64);
}

/// Exact-block-size insertion at a clean boundary: this is the cleanest
/// signal for the prefilter regression - the rolling hash must re-sync at
/// the block boundary immediately after the inserted block.
#[test]
fn block_sized_insertion_at_block_boundary_loses_no_match() {
    for &block_len in BLOCK_SIZES {
        let block_size = block_len as usize;
        for algorithm in matrix_algorithms() {
            assert_aligned_insert_shape(block_len, 1, 1, algorithm);
            assert_aligned_insert_shape(block_len, BASIS_BLOCKS / 2, 1, algorithm);
        }
    }
}

/// MD4 path: the strategy selector emits MD4 only for protocol versions
/// below 30, so this test pins one cell explicitly to keep the pre-30 strong
/// checksum gate covered against the same shifted-insertion adversary.
#[test]
fn md4_pre_protocol_30_strong_checksum_path_round_trips() {
    let block_len: u32 = 1024;
    let block_size = block_len as usize;
    let basis = make_basis(BASIS_BLOCKS * block_size);
    let source = make_source_with_insertion(&basis, 2 * block_size, block_size);

    let (index, script) = run_pipeline(&basis, &source, block_len, SignatureAlgorithm::Md4);
    assert_round_trip(&basis, &source, &index, &script);

    assert_eq!(script.literal_bytes(), block_size as u64);
    assert_eq!(script.copy_bytes(), basis.len() as u64);
}
