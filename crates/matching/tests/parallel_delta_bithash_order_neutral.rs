//! BitHash order-neutrality gate for the opt-in parallel delta scan
//! ([`DeltaGenerator::generate_chunked`]).
//!
//! # What this suite decides
//!
//! `--parallel-delta-scan` stripes the sender-side block-match scan across up to
//! eight rayon workers (`docs/design/intra-file-parallelism.md`, "Approach A").
//! Every stripe probes the *same* signature index, and the coarse gate on that
//! index is the zsync-style BitHash prefilter (`crates/matching/src/index/bithash.rs`):
//! `find_match_*` rejects a source offset outright when `bithash.contains(rsum)`
//! is `false`, before the tag-table and strong-checksum work. Flipping the
//! parallel scan default-ON is only safe if striping the scan cannot change which
//! offsets that prefilter admits or rejects.
//!
//! The sibling suite `parallel_delta_stripe_invariance.rs` pins the *end tokens*
//! (932/933/934). This suite pins the *prefilter layer itself*, so a future
//! BitHash change that broke order-neutrality is caught here specifically, at the
//! gate, and not only as an emergent token divergence.
//!
//! # Why order-neutrality holds by construction
//!
//! A Bloom-style filter's membership answer is independent of insertion order,
//! and here the invariance is stronger than that generic property because of how
//! the index is wired:
//!
//! - The BitHash is built exactly once, in
//!   [`DeltaSignatureIndex::from_signature`] via `populate_index`
//!   (`crates/matching/src/index/builder.rs`), from the full received signature -
//!   before, and independent of, any stripe count.
//! - It is stored as a plain `BitHash` field on the index with *no* interior
//!   mutability. `BitHash::insert`/`clear` take `&mut self`;
//!   `BitHash::contains` takes `&self`.
//! - The parallel path (`generate_chunked_counted` -> `scan_ranges_and_merge`)
//!   hands every rayon worker a shared `&DeltaSignatureIndex`. A worker therefore
//!   *cannot* insert into or clear the BitHash - the shared borrow makes the
//!   `&mut self` mutators unreachable, so the type system forbids a stripe from
//!   making the prefilter stripe-dependent.
//!
//! So striping changes only *which* worker probes *which* offset and in *what*
//! order - never the admit/reject verdict for a given rolling sum. This suite
//! asserts that observable consequence at the prefilter layer.
//!
//! # Properties asserted
//!
//! - **Completeness / one-sidedness** - every indexed full-length block's rolling
//!   sum is admitted by the shared BitHash. This is the decision set that governs
//!   which blocks any stripe can match; a BitHash missing a block silently
//!   demotes every occurrence of it to a literal.
//! - **Admit-decision stability across stripe counts** - the BitHash's admit/reject
//!   verdict, sampled over every indexed block rolling sum plus a fixed spread of
//!   pseudo-random rolling sums, is byte-identical before the scan and after a
//!   forced scan at each stripe count in `{1,2,3,4,5,7,8,16}`. A scan that
//!   rebuilt, cleared, or repopulated the shared filter at any stripe count would
//!   change this fingerprint.
//! - **Prefilter governs matching, invariant to stripes** - the canonical
//!   per-block token stream of the forced parallel scan equals the sequential
//!   scan at every stripe count, and every `Copy` the parallel scan emits sits at
//!   a basis block the shared BitHash admits. A per-stripe-local BitHash (a
//!   plausible "localize the filter" regression) would reject cross-stripe blocks
//!   and diverge the tokens here.
//!
//! These use the test-only [`DeltaGenerator::generate_chunked_forced`] /
//! [`DeltaGenerator::forced_stripe_count`] hooks and the bench-internal
//! [`DeltaSignatureIndex::bithash_admits`] probe, all gated behind
//! `bench-internal` exactly like the sibling suite, so this file compiles only
//! under that feature.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Deterministic LCG byte stream (same generator as the sibling suites).
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
/// checksum, matching the sibling suites.
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

/// A canonical, stripe-shape-independent view of a delta token (see the sibling
/// suite's `canonical` for the rationale: the wire layer expands a coalesced fat
/// `Copy` into one op per basis block, so per-block granularity is the wire
/// contract).
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

/// Stripe counts every fixture is driven through, matching the sibling suite:
/// `1` (single-stripe merge must equal sequential), powers of two up to the
/// production ceiling of 8, the odd counts `3`/`5`/`7` so boundaries bisect a
/// block, and `16` beyond the ceiling.
const STRIPE_COUNTS: &[usize] = &[1, 2, 3, 4, 5, 7, 8, 16];

/// Block length for the fixtures. Small so the fixtures stay modest while still
/// giving many blocks per stripe.
const BLOCK_LEN: u32 = 256;

/// A sampled fingerprint of the BitHash's admit/reject decision function.
///
/// Probes the shared BitHash at every indexed full-length block's rolling sum
/// (the true positives the filter is built to admit) plus a fixed spread of
/// pseudo-random rolling sums (which mostly land on reject bits). Two BitHash
/// states with equal fingerprints agree on every probed verdict; a scan that
/// rebuilt, cleared, or partially repopulated the shared filter would change at
/// least one bit and so the fingerprint.
fn admit_fingerprint(index: &DeltaSignatureIndex) -> Vec<bool> {
    let block_len = index.block_length();
    let mut out = Vec::new();
    // True-positive probes: every indexed full-length block must be admitted.
    for i in 0..index.block_count() {
        let block = index.block(i);
        if block.len() == block_len {
            out.push(index.bithash_admits(block.rolling().value()));
        }
    }
    // Reject-side probes: a fixed spread across the 32-bit rolling-sum space so a
    // partial clear/repopulate of the shared filter perturbs the fingerprint.
    let mut state: u64 = 0xD1CE_F00D_2025_0917;
    for _ in 0..4096 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.push(index.bithash_admits((state >> 32) as u32));
    }
    out
}

/// Absolute source offsets, in block units, of every `Copy` in `script`.
///
/// A fat seq-match `Copy` is expanded to one entry per basis block. The returned
/// offsets let a test assert that every emitted copy sits at a basis block the
/// prefilter admits.
fn copy_basis_indices(script: &DeltaScript, block_len: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for token in script.tokens() {
        if let DeltaToken::Copy { index, len } = token {
            if block_len > 0 && *len > block_len {
                let mut covered = 0usize;
                let mut block = 0u64;
                while covered < *len {
                    out.push(*index + block);
                    covered += block_len.min(*len - covered);
                    block += 1;
                }
            } else {
                out.push(*index);
            }
        }
    }
    out
}

/// Core battery: for a duplicate-free `basis`/`source` pair, asserts the three
/// BitHash order-neutrality properties across every stripe count.
fn assert_bithash_order_neutral(basis: &[u8], source: &[u8], index: &DeltaSignatureIndex) {
    assert!(
        !index.has_duplicate_blocks(),
        "fixture basis must be duplicate-free so the parallel path is wire-transparent"
    );
    let block_len = index.block_length();
    let generator = DeltaGenerator::new();

    // Completeness / one-sidedness: every indexed full-length block is admitted.
    // This is the decision set that governs which blocks any stripe can match.
    for i in 0..index.block_count() {
        let block = index.block(i);
        if block.len() == block_len {
            assert!(
                index.bithash_admits(block.rolling().value()),
                "BitHash must admit every indexed full-length block (block {i})"
            );
        }
    }

    // Snapshot the prefilter's decision function before any scan runs.
    let fingerprint_before = admit_fingerprint(index);

    let sequential = generator
        .generate(Cursor::new(source.to_vec()), index)
        .expect("sequential");
    let seq_canon = canonical(&sequential, block_len);

    // The sequential scan is the production reference; the BitHash's
    // decision function must be unchanged by it.
    assert_eq!(
        admit_fingerprint(index),
        fingerprint_before,
        "the sequential scan must not perturb the shared BitHash"
    );

    for &stripes in STRIPE_COUNTS {
        if source.len() < stripes {
            continue;
        }
        // Non-vacuity of the stripe control: the fixture must genuinely split
        // into the requested number of stripes.
        assert_eq!(
            generator.forced_stripe_count(source.len(), index, stripes),
            stripes,
            "fixture must split into exactly {stripes} stripes"
        );

        let (chunked, _) = generator
            .generate_chunked_forced(source, index, stripes)
            .expect("forced chunked");

        // Admit-decision stability: striping the scan at any count must leave the
        // shared BitHash's verdict on every probed rolling sum byte-identical.
        assert_eq!(
            admit_fingerprint(index),
            fingerprint_before,
            "the parallel scan at {stripes} stripes must not perturb the shared BitHash"
        );

        // Prefilter governs matching, invariant to stripes: the per-block token
        // stream equals the sequential scan's (so the decisions the parallel scan
        // acted on are the sequential ones), and reconstruction is byte-exact.
        assert_eq!(
            canonical(&chunked, block_len),
            seq_canon,
            "canonical token stream must equal the sequential scan at {stripes} stripes"
        );
        assert_eq!(
            reconstruct(basis, index, &chunked),
            source,
            "reconstruction must be byte-identical at {stripes} stripes"
        );

        // Every emitted Copy sits at a basis block the shared BitHash admits: the
        // coarse gate is what let each matched block through, at every count.
        for basis_index in copy_basis_indices(&chunked, block_len) {
            let rsum = index.block(basis_index as usize).rolling().value();
            assert!(
                index.bithash_admits(rsum),
                "every Copy the parallel scan emits must reference a BitHash-admitted \
                 block (basis {basis_index}, {stripes} stripes)"
            );
        }
    }
}

/// Identical source: the stream is overwhelmingly copies, every stripe chains a
/// fat seq-match run that overshoots its boundary, and the BitHash admits every
/// block. The prefilter's decisions must be invariant to how that scan is striped.
#[test]
fn identical_source_bithash_is_order_neutral() {
    let basis = lcg_bytes(0xB17A_50FF_0FDE_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);
    let source = basis.clone();

    assert_bithash_order_neutral(&basis, &source, &index);

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

/// Scattered single-byte edits: most blocks still match at their aligned offset,
/// the edited blocks become literals in both scans. The prefilter's admit set is
/// unchanged and stripe-invariant even as the matched/literal split moves around.
#[test]
fn scattered_edits_bithash_is_order_neutral() {
    let basis = lcg_bytes(0x5CA7_7E5E_D175_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);

    let mut source = basis.clone();
    for &off in &[1_000usize, 9_777, 20_003, 41_111, 55_555, 70_000, 88_888] {
        source[off] ^= 0xa5;
    }

    assert_bithash_order_neutral(&basis, &source, &index);

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

/// Shifted content: an insertion near the front pushes the whole tail off the
/// block grid, so matches land mid-block and stripe boundaries (at counts whose
/// offsets bisect a block) fall inside a match. The overlap read-ahead completes
/// those straddling matches; the prefilter admits the shifted blocks identically
/// regardless of the stripe count that scans them.
#[test]
fn shifted_content_bithash_is_order_neutral() {
    let basis = lcg_bytes(0x5111_7ED0_0FF0_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);

    let insert_at = 4096usize;
    let mut source = Vec::with_capacity(basis.len() + 13);
    source.extend_from_slice(&basis[..insert_at]);
    source.extend_from_slice(b"THIRTEEN-BYTE");
    source.extend_from_slice(&basis[insert_at..]);

    assert_bithash_order_neutral(&basis, &source, &index);

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

/// The BitHash's decision function is a pure property of the built index, so it
/// is identical no matter how many stripes a later scan uses. This pins the
/// contents-identical property directly: the fingerprint sampled from the shared
/// filter does not depend on stripe count, because striping never touches the
/// filter's construction.
#[test]
fn bithash_fingerprint_is_independent_of_stripe_count() {
    let basis = lcg_bytes(0xF1E1_D0A5_1CE0_2025, 96 * 1024);
    let index = build_index(&basis, BLOCK_LEN);
    let source = basis.clone();
    let generator = DeltaGenerator::new();

    let baseline = admit_fingerprint(&index);
    // The fingerprint carries real signal: the true-positive probes are all
    // admits, and the pseudo-random reject-side probes are not all admits (a
    // vector of all-true would make the equality checks vacuous).
    assert!(
        baseline.iter().any(|&admitted| !admitted),
        "reject-side probes must include rejections, or the fingerprint is vacuous"
    );

    for &stripes in STRIPE_COUNTS {
        let (_chunked, _) = generator
            .generate_chunked_forced(&source, &index, stripes)
            .expect("forced chunked");
        assert_eq!(
            admit_fingerprint(&index),
            baseline,
            "BitHash fingerprint must be identical after a {stripes}-stripe scan"
        );
    }
}
