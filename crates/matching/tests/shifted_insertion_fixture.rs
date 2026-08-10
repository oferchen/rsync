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
    let checksum_len = u8::try_from(algorithm.digest_len()).expect("digest fits in u8");
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(block_len).expect("block_len non-zero")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(checksum_len).expect("checksum length non-zero"),
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
    assert_eq!(
        output, source,
        "round-trip reconstruction must match source"
    );
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

    // The seq-match optimisation may coalesce consecutive matched basis
    // blocks into a single fat `Copy { len = run * block_len }`. Expand
    // back to per-block indices for the contiguity invariant assertion.
    let mut copy_indices: Vec<u64> = Vec::new();
    for tok in script.tokens() {
        if let DeltaToken::Copy { index, len } = tok {
            let run = (*len / block_size).max(1);
            for k in 0..run {
                copy_indices.push(*index + k as u64);
            }
        }
    }
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
    // block 0; the subsequent COPY bytes must run contiguously to the end.
    // Expand fat Copy tokens (seq-match coalesces consecutive matches).
    let block_size = block_len as usize;
    let mut copy_sequence: Vec<u64> = Vec::new();
    for tok in script.tokens() {
        if let DeltaToken::Copy { index, len } = tok {
            let run = (*len / block_size).max(1);
            for k in 0..run {
                copy_sequence.push(*index + k as u64);
            }
        }
    }
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

    let (index, script) = run_pipeline(
        &basis,
        &source,
        block_len,
        SignatureAlgorithm::Xxh3 { seed: 0 },
    );
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

/// Basis size for the 1 MB shifted-insertion adversary. Sized at exactly one
/// mebibyte so the basis spans 1024 blocks at `LARGE_BLOCK_LEN`, which is well
/// above the rsum-bucket and bithash-saturation thresholds. The bigger the
/// basis, the harder it is for a purely tag-table-based prefilter to keep
/// rejection rates high, which is exactly the regime the bithash and seq-match
/// optimisations target. See `project_zsync_optimizations.md` for the planned
/// optimisation surface this fixture guards.
const LARGE_BASIS_LEN: usize = 1024 * 1024;

/// Block length for the 1 MB shifted-insertion runs. 1024 lands on a clean
/// power-of-two boundary that lets `LARGE_BASIS_LEN / LARGE_BLOCK_LEN` divide
/// cleanly, so we can express insert positions as integer block multiples
/// without rounding artefacts.
const LARGE_BLOCK_LEN: u32 = 1024;

/// xorshift64* PRNG core. Deterministic, full 2^64 - 1 period, no allocations,
/// no external crate. Used to generate the 1 MB basis without committing the
/// raw bytes to the test tree.
#[inline]
fn xorshift64_star(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Builds a deterministic basis of `len` bytes from a seeded xorshift64* PRNG.
/// Two callers with the same `seed` produce byte-identical output; different
/// seeds produce independent streams.
fn make_basis_xorshift(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1; // xorshift seed must be non-zero
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let word = xorshift64_star(&mut state).to_le_bytes();
        let take = (len - out.len()).min(word.len());
        out.extend_from_slice(&word[..take]);
    }
    out
}

/// Builds a filler region whose bytes cannot match any window in a basis
/// produced by [`make_basis_xorshift`] with the same seed: every filler byte
/// has its high bit set, while the xorshift64* output produces every bit
/// uniformly. Although there is a 1/256 chance any single basis byte happens
/// to also have the high bit set, no full `block_len`-byte window of the
/// inserted run will collide with the basis hash because the rolling
/// checksum's strong-checksum gate rejects unrelated content with probability
/// essentially equal to `2^(-strong_len*8)`.
fn make_insertion_filler(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let word = xorshift64_star(&mut state).to_le_bytes();
        for &b in &word[..(len - out.len()).min(word.len())] {
            out.push(b | 0x80);
        }
    }
    out
}

/// One row of the parameterised shifted-insertion fixture vector. Each row
/// drives a single basis/source pair through the delta pipeline and exercises
/// a specific `(N, M)` combination against the 1 MB basis.
struct ShiftedInsertionCase {
    /// Human-readable label used in assertion messages so failures point
    /// directly at the offending row of the fixture vector.
    label: &'static str,
    /// Insert offset in bytes. Always `<= LARGE_BASIS_LEN`.
    m: usize,
    /// Insert length in bytes. The resulting source is `LARGE_BASIS_LEN + n`
    /// bytes long.
    n: usize,
}

/// Asserts the delta references basis blocks both before and after the
/// insertion point, proving the rolling-hash re-synchronised across the
/// shift. The list of expected block indices is split into "before" (block
/// indices `< m / block_len`) and "after" (block indices `>= ceil(m /
/// block_len)`). For an aligned insert both halves are contiguous; for an
/// unaligned insert at most one mid-stride block at the boundary can be
/// dropped, which the assertion explicitly tolerates.
fn assert_matches_span_insertion(
    case: &ShiftedInsertionCase,
    script: &DeltaScript,
    basis_len: usize,
    block_len: usize,
) {
    let total_blocks = basis_len / block_len;
    let boundary_block = case.m / block_len;

    let mut copy_indices: Vec<u64> = Vec::new();
    for tok in script.tokens() {
        if let DeltaToken::Copy { index, len } = tok {
            let run = (*len / block_len).max(1);
            for k in 0..run {
                copy_indices.push(*index + k as u64);
            }
        }
    }

    let before: Vec<u64> = copy_indices
        .iter()
        .copied()
        .filter(|&idx| (idx as usize) < boundary_block)
        .collect();
    let after: Vec<u64> = copy_indices
        .iter()
        .copied()
        .filter(|&idx| (idx as usize) >= boundary_block)
        .collect();

    assert!(
        !before.is_empty() || boundary_block == 0,
        "{}: expected matches before the insertion (M={}, boundary_block={})",
        case.label,
        case.m,
        boundary_block,
    );
    assert!(
        !after.is_empty() || boundary_block == total_blocks,
        "{}: expected matches after the insertion (M={}, boundary_block={}, total_blocks={})",
        case.label,
        case.m,
        boundary_block,
        total_blocks,
    );

    // Combined coverage: the rolling hash must survive the shift well enough
    // that we lose at most one boundary block. Anything worse points at a
    // prefilter regression that drops aligned matches after a shift.
    let lost = total_blocks.saturating_sub(copy_indices.len());
    assert!(
        lost <= 1,
        "{}: lost {lost} blocks across the shift, expected at most 1 \
         (M={}, N={}, before={}, after={})",
        case.label,
        case.m,
        case.n,
        before.len(),
        after.len(),
    );
}

/// 1 MB shifted-insertion fixture vector. The `(N, M)` combinations cover:
///
/// - Block-aligned insertions at the start, middle, and tail of the basis.
/// - Unaligned insertions at sub-block, sub-word, and tail-edge offsets.
/// - Multi-block insertions large enough to span a full bithash-bucket
///   refill window.
///
/// Every row reuses the same 1 MB basis (built once per case) to keep
/// allocation pressure bounded; the pipeline rebuilds its index from
/// the signature on each call.
#[test]
fn shifted_insertion_1mib_basis_matches_span_the_shift() {
    let block_len = LARGE_BLOCK_LEN;
    let block_size = block_len as usize;
    let basis = make_basis_xorshift(LARGE_BASIS_LEN, 0xC0FF_EE15_BAAD_F00D);
    assert_eq!(basis.len(), LARGE_BASIS_LEN);

    let cases = [
        ShiftedInsertionCase {
            label: "head-aligned-1block",
            m: 0,
            n: block_size,
        },
        ShiftedInsertionCase {
            label: "mid-aligned-1block",
            m: (LARGE_BASIS_LEN / 2) - ((LARGE_BASIS_LEN / 2) % block_size),
            n: block_size,
        },
        ShiftedInsertionCase {
            label: "mid-aligned-4blocks",
            m: (LARGE_BASIS_LEN / 4) - ((LARGE_BASIS_LEN / 4) % block_size),
            n: 4 * block_size,
        },
        ShiftedInsertionCase {
            label: "near-tail-aligned",
            m: LARGE_BASIS_LEN - 2 * block_size,
            n: block_size,
        },
        ShiftedInsertionCase {
            label: "mid-unaligned-1byte",
            m: (LARGE_BASIS_LEN / 2) + 1,
            n: 1,
        },
        ShiftedInsertionCase {
            label: "mid-unaligned-7bytes",
            m: (LARGE_BASIS_LEN / 3) + 13,
            n: 7,
        },
        ShiftedInsertionCase {
            label: "quarter-unaligned-large",
            m: (LARGE_BASIS_LEN / 4) + (block_size / 2),
            n: 3 * block_size + 5,
        },
    ];

    let algorithm = SignatureAlgorithm::Md5 {
        seed_config: Md5Seed::none(),
    };
    let index = build_index(&basis, block_len, algorithm).expect("index built for 1 MB basis");

    for case in &cases {
        let mut source = Vec::with_capacity(LARGE_BASIS_LEN + case.n);
        source.extend_from_slice(&basis[..case.m]);
        source.extend(make_insertion_filler(case.n, 0xDEAD_BEEF_1234_5678));
        source.extend_from_slice(&basis[case.m..]);

        let script = generate_delta(Cursor::new(&source), &index)
            .unwrap_or_else(|err| panic!("{}: delta generation failed: {err}", case.label));

        assert_eq!(
            script.total_bytes(),
            source.len() as u64,
            "{}: total bytes",
            case.label,
        );

        assert!(
            script.literal_bytes() >= case.n as u64,
            "{}: literal_bytes={} must cover at least the {} inserted bytes",
            case.label,
            script.literal_bytes(),
            case.n,
        );

        assert_round_trip(&basis, &source, &index, &script);
        assert_matches_span_insertion(case, &script, basis.len(), block_size);
    }
}
