//! Stripe-invariance gate suite for the opt-in parallel delta scan
//! ([`DeltaGenerator::generate_chunked`]).
//!
//! # What this suite decides
//!
//! `--parallel-delta-scan` stripes the sender-side block-match scan across up to
//! eight rayon workers, each scanning an overlapping range of the source against
//! a shared read-only signature index, then merges the per-stripe `Copy` runs
//! back into the single-pass token stream
//! (`docs/design/intra-file-parallelism.md`, "Approach A"). It is default-OFF.
//! Flipping it default-ON is only safe if the parallel scan is proven byte-exact
//! and wire-transparent versus the sequential scan. This suite is the gate that
//! decides that: it asserts the merged output is invariant to the stripe count.
//!
//! The production entry point [`DeltaGenerator::generate_chunked`] derives its
//! stripe count from the source size, so it cannot be driven at a chosen count
//! on a modest fixture. These tests use the test-only
//! [`DeltaGenerator::generate_chunked_forced`], which runs the identical
//! overlap + greedy-merge machinery at an exact requested stripe count. That
//! entry never exists in a release build (it is gated behind `bench-internal`,
//! like the crate's other test-only knobs), so this file is compiled only under
//! that feature.
//!
//! # Properties asserted
//!
//! - **Reconstruction identity** - applying the delta produced at every stripe
//!   count reconstructs the source byte-identically from the basis.
//! - **Token-sequence identity** - the canonical (per-block-normalized) token
//!   stream is identical across all stripe counts, and equal to the sequential
//!   scan's. This is strictly stronger than reconstruction identity, and it is
//!   the wire-transparency contract: the wire layer expands a fat seq-match
//!   `Copy` into one op per block, so two scripts with the same per-block token
//!   sequence serialize to the same wire bytes
//!   (upstream: `match.c:hash_search()`, single monotone token cursor;
//!   `docs/design/intra-file-parallelism.md` wire-compat invariant 1).
//! - **N=1 through the merge equals sequential** - forcing a single stripe still
//!   routes through `scan_ranges_and_merge`, so this proves the merge of one
//!   stripe reproduces the sequential scan (not the `chunks <= 1` shortcut,
//!   which would bypass the merge entirely and prove nothing).
//! - **Boundary-spanning correctness** - a matching block that straddles a
//!   stripe boundary is completed by the owning worker's read-ahead and merged
//!   without loss or duplication, for stripe counts whose boundaries bisect a
//!   block.
//! - **Duplicate-content divergence** - on a duplicate-heavy basis the prune-off
//!   parallel scan resolves siblings differently and the token stream diverges;
//!   reconstruction stays exact. This pins why the production wiring gates the
//!   parallel path on a duplicate-free basis (the fallback itself lives in
//!   `transfer::generator::generate_delta_from_signature_chunked`).
//! - **Non-vacuity** - a corrupted merge (a dropped boundary `Copy`, or two
//!   tokens mis-ordered) must fail both reconstruction identity and
//!   token-sequence identity, proving the assertions have teeth.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Deterministic LCG byte stream (same generator as `parallel_delta_wire_parity.rs`).
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

/// A canonical, stripe-shape-independent view of a delta token.
///
/// The wire layer expands a coalesced fat `Copy` (`len = run * block_len`) into
/// one op per basis block, so token-sequence identity must be judged at
/// per-block granularity, not on the coalesced token shape. This normalizer
/// splits every `Copy` into its constituent per-block copies (the trailing short
/// block keeps its true length) and records literals by their byte content. Two
/// scripts with equal canonical vectors serialize to identical wire bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CanonToken {
    Copy { index: u64, len: usize },
    Literal(Vec<u8>),
}

/// Normalizes a [`DeltaScript`] into its canonical per-block token vector.
fn canonical(script: &DeltaScript, block_len: usize) -> Vec<CanonToken> {
    let mut out = Vec::new();
    for token in script.tokens() {
        match token {
            DeltaToken::Literal(bytes) => out.push(CanonToken::Literal(bytes.clone())),
            DeltaToken::Copy { index, len } => {
                if block_len > 0 && *len > block_len {
                    let mut covered = 0usize;
                    let mut block = 0u64;
                    while covered < *len {
                        let this = block_len.min(*len - covered);
                        out.push(CanonToken::Copy {
                            index: *index + block,
                            len: this,
                        });
                        covered += this;
                        block += 1;
                    }
                } else {
                    out.push(CanonToken::Copy {
                        index: *index,
                        len: *len,
                    });
                }
            }
        }
    }
    out
}

/// Reconstructs the source from `basis` by applying `script`.
fn reconstruct(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript) -> Vec<u8> {
    let mut cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::new();
    apply_delta(&mut cursor, &mut output, index, script).expect("apply");
    output
}

/// Stripe counts the suite drives every duplicate-free fixture through.
///
/// Includes `1` (the merge of a single stripe must equal the sequential scan),
/// powers of two up to the production ceiling of 8, and `16` (beyond the ceiling
/// so a heavier boundary count is exercised). The odd counts `3`/`5`/`7` are
/// added by the boundary fixture so stripe boundaries bisect a block.
const STRIPE_COUNTS: &[usize] = &[1, 2, 3, 4, 5, 7, 8, 16];

/// Block length for the fixtures. Small so the fixtures stay modest while still
/// giving many blocks per stripe (fixture bytes / block_len >> stripe count).
const BLOCK_LEN: u32 = 256;

/// Runs the full duplicate-free invariance battery for one `source`/`basis`
/// pair against `index`, and returns the sequential baseline script.
///
/// Asserts, for every count in [`STRIPE_COUNTS`]:
///  * the fixture genuinely splits into that many stripes (non-vacuity of the
///    stripe control),
///  * the merged reconstruction equals `source` (932),
///  * the canonical token vector equals the sequential scan's (933 + 934).
fn assert_dup_free_invariant(basis: &[u8], source: &[u8], index: &DeltaSignatureIndex) {
    assert!(
        !index.has_duplicate_blocks(),
        "fixture basis must be duplicate-free so the parallel path is wire-transparent"
    );
    let block_len = index.block_length();
    let generator = DeltaGenerator::new();

    let sequential = generator
        .generate(Cursor::new(source.to_vec()), index)
        .expect("sequential");
    let seq_canon = canonical(&sequential, block_len);
    assert_eq!(
        reconstruct(basis, index, &sequential),
        source,
        "sequential baseline must reconstruct the source"
    );

    for &stripes in STRIPE_COUNTS {
        if source.len() < stripes {
            continue;
        }
        // Non-vacuity of the stripe control: the fixture must actually split
        // into the requested number of stripes, not collapse to one range.
        assert_eq!(
            generator.forced_stripe_count(source.len(), index, stripes),
            stripes,
            "fixture must split into exactly {stripes} stripes"
        );

        let (chunked, _) = generator
            .generate_chunked_forced(source, index, stripes)
            .expect("forced chunked");

        // 932: reconstruction identity.
        assert_eq!(
            reconstruct(basis, index, &chunked),
            source,
            "reconstruction must be byte-identical at {stripes} stripes"
        );

        // 933 / 934: token-sequence identity vs the sequential scan.
        assert_eq!(
            canonical(&chunked, block_len),
            seq_canon,
            "canonical token stream must equal the sequential scan at {stripes} stripes"
        );
    }
}

/// Fixture (b): a large-enough source that genuinely stripes across every count,
/// identical to the basis so the stream is overwhelmingly copies and every
/// stripe chains a fat seq-match run that overshoots its boundary. The merge
/// must reassemble those overlapping runs at block granularity.
#[test]
fn identical_source_is_stripe_invariant() {
    let basis = lcg_bytes(0xC0FF_EE00_1DED_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);
    let source = basis.clone();

    assert_dup_free_invariant(&basis, &source, &index);

    // Guard against a trivially-all-literal pass masquerading as success.
    let generator = DeltaGenerator::new();
    let (chunked, _) = generator
        .generate_chunked_forced(&source, &index, 8)
        .expect("forced chunked");
    assert!(
        chunked.copy_bytes() > (source.len() as u64) * 9 / 10,
        "identical source must be overwhelmingly copies (copy_bytes={})",
        chunked.copy_bytes()
    );
}

/// Fixture (c): a diff with scattered small edits. Most blocks still match at
/// their aligned offset; the edited blocks become literals in both scans. The
/// token framing across every stripe boundary must line up.
#[test]
fn scattered_edits_are_stripe_invariant() {
    let basis = lcg_bytes(0x5CA7_7E5E_D175_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);

    let mut source = basis.clone();
    // Flip one byte inside a scattering of blocks spread across the file.
    for &off in &[1_000usize, 9_777, 20_003, 41_111, 55_555, 70_000, 88_888] {
        source[off] ^= 0xa5;
    }

    assert_dup_free_invariant(&basis, &source, &index);

    // The edits must leave the bulk matched, or the test is trivially satisfied.
    let generator = DeltaGenerator::new();
    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    assert!(
        sequential.copy_bytes() > (source.len() as u64) * 8 / 10,
        "scattered edits must leave the bulk matched (copy_bytes={})",
        sequential.copy_bytes()
    );
}

/// Fixture (d) / boundary-spanning (935): shifted content so every block after
/// the insertion matches the basis off the block grid, and the stripe boundaries
/// (chosen at counts whose boundary offsets bisect a block) land mid-match. This
/// is exactly the overlap-region correctness the per-stripe read-ahead exists
/// for: a block-aligned split with no overlap would drop these straddling
/// matches to literals.
#[test]
fn shifted_content_crossing_boundaries_is_stripe_invariant() {
    let basis = lcg_bytes(0x5111_7ED0_0FF0_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);

    // Insert 13 bytes near the front so the whole tail shifts +13 off the grid.
    let insert_at = 4096usize;
    let mut source = Vec::with_capacity(basis.len() + 13);
    source.extend_from_slice(&basis[..insert_at]);
    source.extend_from_slice(b"THIRTEEN-BYTE");
    source.extend_from_slice(&basis[insert_at..]);

    assert_dup_free_invariant(&basis, &source, &index);

    let generator = DeltaGenerator::new();
    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    assert!(
        sequential.copy_bytes() > (basis.len() as u64) / 2,
        "shifted content must still match the bulk of the basis (copy_bytes={})",
        sequential.copy_bytes()
    );
}

/// Fixture (a): a source below the production parallel threshold. The production
/// [`DeltaGenerator::generate_chunked`] must collapse to the sequential scan
/// (the `chunks <= 1` shortcut), and forcing a single stripe through the merge
/// must also equal the sequential scan.
#[test]
fn small_source_collapses_and_single_stripe_equals_sequential() {
    let basis = lcg_bytes(0x0A11_5A11_1111_2025, 8 * 1024);
    let index = build_index(&basis, BLOCK_LEN);
    let source = basis.clone();
    let block_len = index.block_length();
    let generator = DeltaGenerator::new();

    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    let seq_canon = canonical(&sequential, block_len);

    // Production entry: below the 1 MiB / 64-block floor, so even requesting 8
    // chunks collapses to the sequential scan.
    let (production, _) = generator
        .generate_chunked_forced(&source, &index, 1)
        .expect("forced single stripe");
    // Forcing exactly one stripe still runs the merge (not the shortcut) and
    // must reproduce the sequential token stream.
    assert_eq!(
        canonical(&production, block_len),
        seq_canon,
        "single-stripe merge must equal the sequential scan"
    );
    assert_eq!(reconstruct(&basis, &index, &production), source);

    // The unforced production path collapses this small source to one range.
    let chunked = generator
        .generate_chunked(&source, &index, 8)
        .expect("chunked");
    assert_eq!(
        canonical(&chunked, block_len),
        seq_canon,
        "small source must collapse to the sequential scan"
    );
}

/// Duplicate-content basis (fixture e): the prune-off parallel scan resolves
/// duplicate siblings differently from the pruned sequential scan, so the token
/// stream diverges - but reconstruction stays byte-exact. This pins the boundary
/// the duplicate-free gate exists to avoid; the production fallback itself lives
/// in `transfer::generator::generate_delta_from_signature_chunked` and is pinned
/// there.
#[test]
fn duplicate_basis_diverges_but_reconstructs() {
    let block_a = lcg_bytes(0x0DDB_A5E5_CAB7_E5A0, BLOCK_LEN as usize);
    let block_b = lcg_bytes(0x0DDB_A5E5_CAB7_E5B0, BLOCK_LEN as usize);
    let block_c = lcg_bytes(0x0DDB_A5E5_CAB7_E5C0, BLOCK_LEN as usize);

    let mut basis = Vec::new();
    while basis.len() < 96 * 1024 {
        basis.extend_from_slice(&block_a);
        basis.extend_from_slice(&block_b);
        basis.extend_from_slice(&block_c);
    }
    let index = build_index(&basis, BLOCK_LEN);
    assert!(
        index.has_duplicate_blocks(),
        "A/B/C repetition must be flagged duplicate-heavy"
    );

    let source = basis.clone();
    let block_len = index.block_length();
    let generator = DeltaGenerator::new();

    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    let (chunked, _) = generator
        .generate_chunked_forced(&source, &index, 8)
        .expect("forced chunked");

    // Both reconstruct exactly: divergence is in token shape, not correctness.
    assert_eq!(reconstruct(&basis, &index, &sequential), source);
    assert_eq!(reconstruct(&basis, &index, &chunked), source);

    assert_ne!(
        canonical(&chunked, block_len),
        canonical(&sequential, block_len),
        "duplicate-heavy basis must diverge in token shape; this is exactly why the \
         production wiring gates the parallel path on a duplicate-free basis"
    );
}

/// Non-vacuity (938): the suite's instruments (reconstruction identity and
/// token-sequence identity) must FAIL on a corrupted merge. Two corruptions
/// model the two ways the merge can go wrong - a dropped boundary `Copy` (the
/// overlap region lost) and two tokens mis-ordered (per-stripe concatenation out
/// of order). Both must be caught, proving the passing assertions above are not
/// vacuous.
#[test]
fn corrupted_merge_is_detected() {
    let basis = lcg_bytes(0xBEEF_F00D_1234_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);
    // Scattered edits give an alternating Copy/Literal stream with many distinct
    // adjacent tokens, so both corruptions below have something to bite on.
    let mut source = basis.clone();
    for &off in &[2_048usize, 12_345, 24_680, 48_000, 60_001, 72_222, 84_444] {
        source[off] ^= 0x5a;
    }
    let source = source;
    let block_len = index.block_length();
    let generator = DeltaGenerator::new();

    let sequential = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("sequential");
    let seq_canon = canonical(&sequential, block_len);

    let (chunked, _) = generator
        .generate_chunked_forced(&source, &index, 8)
        .expect("forced chunked");
    // Baseline: the honest merge passes both instruments.
    assert_eq!(reconstruct(&basis, &index, &chunked), source);
    assert_eq!(canonical(&chunked, block_len), seq_canon);

    let mut tokens = chunked.tokens().to_vec();
    // Find a Copy to corrupt (there are many in an identical-source stream).
    let copy_pos = tokens
        .iter()
        .position(|t| matches!(t, DeltaToken::Copy { .. }))
        .expect("stream has copies");

    // Corruption 1: drop a boundary Copy (overlap region lost). Reconstruction
    // must no longer equal the source, and the token stream must differ.
    let mut dropped = tokens.clone();
    dropped.remove(copy_pos);
    let dropped_script = DeltaScript::new(
        dropped.clone(),
        source.len() as u64,
        dropped
            .iter()
            .filter(|t| t.is_literal())
            .map(|t| t.byte_len() as u64)
            .sum(),
    );
    assert_ne!(
        reconstruct(&basis, &index, &dropped_script),
        source,
        "dropping a merge Copy must break reconstruction identity"
    );
    assert_ne!(
        canonical(&dropped_script, block_len),
        seq_canon,
        "dropping a merge Copy must break token-sequence identity"
    );

    // Corruption 2: mis-order two adjacent tokens (per-stripe concat reordered).
    // Pick two adjacent tokens that are not equal so the swap is observable.
    let swap_at = tokens
        .windows(2)
        .position(|w| w[0] != w[1])
        .expect("stream has two differing adjacent tokens");
    tokens.swap(swap_at, swap_at + 1);
    let reordered = DeltaScript::new(
        tokens.clone(),
        source.len() as u64,
        tokens
            .iter()
            .filter(|t| t.is_literal())
            .map(|t| t.byte_len() as u64)
            .sum(),
    );
    assert_ne!(
        canonical(&reordered, block_len),
        seq_canon,
        "mis-ordering merge tokens must break token-sequence identity"
    );
}
