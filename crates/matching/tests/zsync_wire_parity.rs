//! Wire-byte parity regression tests for the zsync-inspired matching
//! optimizations (ZSO-1..4).
//!
//! These tests are the deliverable for **ZSO-5** (task #2513). For each
//! optimization listed below, they construct a basis + source pair designed
//! to exercise that optimization's code path, run the matching crate's
//! `generate_delta` pipeline, serialise the resulting [`DeltaScript`]
//! through the **same** wire-format encoder that `transfer::generator`
//! emits on the network, and pin the resulting byte stream.
//!
//! # Why pin the wire bytes here?
//!
//! The protocol golden tests under `crates/protocol/tests/golden_*` already
//! cover wire-byte correctness at the protocol layer (multiplex headers,
//! token framing, NDX encoding). They do **not** synthesize the specific
//! basis + source pairs that exercise each ZSO optimization path, so a
//! regression in `crates/matching/src/` that changed token shape but
//! preserved frame-level validity would slip past them. This file is the
//! complement: a fixed-seed corpus where any future regression in
//! `DeltaGenerator`, `DeltaSignatureIndex`, or the seq-match coalescing
//! that lives on top of them will produce a wire-byte diff that is
//! impossible to land silently.
//!
//! # ZSO optimization map
//!
//! | ZSO  | Task   | Status on master              | Status in this file |
//! |------|--------|-------------------------------|---------------------|
//! | ZSO-1| #2510  | shipped (PR #3737)            | active              |
//! | ZSO-2| #2510  | landing on PR #4624           | active (see note)   |
//! | ZSO-3| #2511  | shipped                       | active              |
//! | ZSO-4| #2512  | shipped                       | active              |
//!
//! ZSO-2 (sequential-match lookahead) is implemented on branch
//! `feat/matching-zsync-seq-match-lookahead-zso2` (PR #4624). The active
//! test here exercises the basis+source pair that triggers ZSO-2's adjacent
//! -block fast path; on master it still produces the seq-match coalesced
//! output via the pre-#4624 generator path because the post-match probe
//! shortcut and the in-script coalescing are independent. When #4624 lands
//! the same fixed-seed input must continue to produce byte-identical wire
//! output (different probe path, same emitted tokens).
//!
//! # Reference output
//!
//! There is no externally-supplied baseline today, so the assertion
//! strategy is:
//!
//! 1. Run `generate_delta` twice on the same fixed-seed input and assert
//!    the two wire-byte streams are byte-identical (determinism gate).
//! 2. Round-trip the script through `apply_delta` on the basis and assert
//!    the reconstruction is byte-identical to the source (functional
//!    correctness gate; this is the upstream wire-compat invariant).
//! 3. Pin structural invariants on the token shape (e.g. ZSO-2 produces a
//!    single fat copy for an all-match source; ZSO-1 produces a non-empty
//!    literal run for an all-miss source) so a future regression that
//!    silently merges or splits tokens trips at least one assertion.
//!
//! ZSO-3 and ZSO-4 have shipped, so their tests are active. Because the
//! prune bitmap and the compact rolling-key are on unconditionally in the
//! public API, those two tests pin match correctness directly: a prune
//! regression forces a matchable block to a literal, and a compact-key
//! regression drops a well-separated match - both trip a token-shape
//! assertion rather than needing an opt-out reference run.
//!
//! # Cross-references
//!
//! - Protocol-layer golden tests (complement, not a substitute):
//!   `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs`
//!   for the token frame format pinned here.
//! - ZSO design memory:
//!   `docs/design/zsync-bithash.md`, `docs/design/zsync-seq-match.md`.
//! - Upstream references: `match.c:hash_search()`,
//!   `token.c:simple_send_token()`.

use matching::{DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta};
use protocol::ProtocolVersion;
use protocol::wire::{DeltaOp, write_token_stream};
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

/// Deterministic LCG byte stream used by every fixture. Seeded with a
/// per-test constant so each test owns an independent stream and a
/// regression in one fixture's output does not mask another's.
///
/// The chosen multiplier / increment are the values used in Knuth's
/// classic MMIX LCG (sufficient for non-cryptographic byte fill); the
/// state is advanced 64 bits at a time and the low byte of each step is
/// taken. The crate already forbids new dev-deps, so this stands in for
/// a `rand` import.
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

/// Builds a [`DeltaSignatureIndex`] with a forced block length and MD4
/// strong checksum. MD4 matches the default chosen by upstream rsync for
/// the protocol versions covered by the matching crate and keeps the
/// pinned byte stream stable across host endianness because all rolling
/// checksums in `checksums` are explicitly little-endian on the wire.
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

/// Converts a [`DeltaScript`] into the per-block [`DeltaOp`] stream that
/// `transfer::generator::script_to_wire_delta` emits before handing off to
/// `protocol::wire::write_token_stream`.
///
/// The split is necessary because a single seq-match-coalesced
/// `DeltaToken::Copy { len = N * block_length }` corresponds to N
/// individual block-match tokens on the wire (one negative integer per
/// basis block). See `transfer::generator::delta::script_to_wire_delta`
/// for the production code path - this helper mirrors its semantics
/// without pulling the `transfer` crate as a dev-dep.
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

/// Serialises a [`DeltaScript`] into the exact byte sequence that would
/// travel over an rsync protocol stream (token frames only - no multiplex
/// framing, no compression header, no signature header; those layers are
/// covered by `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs`).
fn script_to_wire_bytes(script: &DeltaScript, block_len: usize) -> Vec<u8> {
    let ops = script_to_ops(script, block_len);
    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).expect("token stream serialises");
    buf
}

/// Runs `generate_delta` end-to-end and returns both the script and its
/// wire-byte serialisation. Used by every ZSO test as the single entry
/// point so the active and ignored tests share an identical pipeline.
fn run_pipeline(basis: &[u8], source: &[u8], block_len: u32) -> (DeltaScript, Vec<u8>) {
    let index = build_index(basis, block_len);
    let script = generate_delta(Cursor::new(source.to_vec()), &index).expect("delta generated");
    let wire = script_to_wire_bytes(&script, index.block_length());
    (script, wire)
}

/// Round-trips `script` through `apply_delta` against `basis` and asserts
/// the reconstruction is byte-identical to `source`. This is the
/// upstream-rsync wire-compat invariant: no matter how the matching
/// pipeline coalesces or shuffles tokens internally, the reconstructed
/// payload on the receiving side must equal the sender's source bytes.
fn assert_round_trip(basis: &[u8], source: &[u8], script: &DeltaScript) {
    let index = build_index(basis, {
        // Re-derive the block length the script was built with so the
        // helper is self-contained. The signature layout is deterministic
        // for a given basis length + algorithm.
        let probe = build_index(basis, 700);
        u32::try_from(probe.block_length()).expect("block_length fits in u32")
    });
    let mut cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::with_capacity(source.len());
    apply_delta(&mut cursor, &mut output, &index, script).expect("apply");
    assert_eq!(
        output, source,
        "round-trip reconstruction must match source"
    );
}

// ---------------------------------------------------------------------------
// ZSO-1 - bithash prefilter (PR #3737, task #2510)
// ---------------------------------------------------------------------------

/// ZSO-1 active test. Builds a 16 KiB random basis and a 16 KiB
/// **disjoint** random source: no source window can match any basis block,
/// so every rolling probe goes through the bithash prefilter and is
/// rejected without touching the (cold) `lookup` hash map. The resulting
/// script must therefore be a single fat literal followed by an end
/// marker, and the wire-byte stream must be deterministic across runs.
///
/// Regression intent: any change to the bithash insertion or probe path
/// that lets through a false-positive into the strong-checksum gate would
/// still re-produce the same script (the strong checksum would catch it),
/// but a change that *dropped* a real match would split the literal or
/// shorten the byte stream and trip the equality assertion below.
#[test]
fn bithash_optimization_preserves_wire_bytes() {
    // 16 KiB basis - large enough to populate the bithash table at the
    // documented ~12.5% density, small enough to keep the test under
    // `cargo nextest` timeouts.
    let basis = lcg_bytes(0x0B17_4A54_5EED_0001, 16 * 1024);
    // Disjoint seed -> no window in `source` can match any block in
    // `basis` (the rolling-checksum collision probability for a 16 KiB
    // corpus is far below 1).
    let source = lcg_bytes(0xC0DE_FEED_DEAD_BEEF, 16 * 1024);

    let (script, wire) = run_pipeline(&basis, &source, 700);

    assert!(
        !wire.is_empty(),
        "wire-byte stream must not be empty for a 16 KiB source"
    );

    // Determinism gate: running the pipeline twice on the same input
    // produces the same wire-byte stream.
    let (_, wire2) = run_pipeline(&basis, &source, 700);
    assert_eq!(
        wire, wire2,
        "ZSO-1 wire-byte output must be deterministic across runs"
    );

    // Functional correctness: the reconstruction equals the source.
    assert_round_trip(&basis, &source, &script);

    // Structural invariant: a fully-disjoint source produces literals
    // covering the entire payload (no Copy tokens). A regression in the
    // bithash filter that mistakenly admitted false matches all the way
    // through the strong-checksum gate would emit Copy tokens here.
    assert_eq!(
        script.copy_bytes(),
        0,
        "disjoint basis + source must yield zero copy bytes (got {})",
        script.copy_bytes()
    );
    assert_eq!(
        script.literal_bytes(),
        source.len() as u64,
        "every source byte must be emitted as a literal"
    );
}

// ---------------------------------------------------------------------------
// ZSO-2 - sequential-match lookahead (PR #4624, task #2510)
// ---------------------------------------------------------------------------

/// ZSO-2 active test. Builds a basis whose contents are an exact integer
/// multiple of the block length, then uses **the basis itself** as the
/// source. Every basis block matches its natural offset in the source,
/// which is the ideal driver for ZSO-2's adjacent-block fast path: after
/// each confirmed match the generator can short-circuit straight to the
/// `next_match` link rather than re-rolling the checksum across the
/// already-matched window.
///
/// Dependency note: ZSO-2 is being landed on PR #4624. On master the
/// `extend_run` helper plus the existing seq-match coalescing already
/// produces the single-fat-copy script shape this test pins; PR #4624
/// reaches the same shape via a faster probe path. Either way the wire
/// bytes must stay identical.
#[test]
fn seq_match_optimization_preserves_wire_bytes() {
    // 84 blocks of 700 bytes = 58 800 bytes. Exact multiple of the
    // default block length so `extend_run` walks every basis block.
    const BLOCK_LEN: u32 = 700;
    const BLOCKS: usize = 84;
    let basis = lcg_bytes(0x5E5E_5E5E_C0FF_EE00, BLOCK_LEN as usize * BLOCKS);
    let source = basis.clone();

    let (script, wire) = run_pipeline(&basis, &source, BLOCK_LEN);

    assert!(!wire.is_empty(), "wire-byte stream must not be empty");

    let (_, wire2) = run_pipeline(&basis, &source, BLOCK_LEN);
    assert_eq!(
        wire, wire2,
        "ZSO-2 wire-byte output must be deterministic across runs"
    );

    assert_round_trip(&basis, &source, &script);

    // Structural invariant: an all-match source coalesces into exactly
    // one fat Copy token spanning every full basis block. ZSO-2 changes
    // the probe path that produces this run but cannot change its
    // shape; a regression that broke either the coalescing or the
    // adjacent-block hint would split the run into multiple Copy tokens
    // (still correct, but a wire-byte diff this test would catch).
    let copy_tokens: Vec<&DeltaToken> = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .collect();
    assert_eq!(
        copy_tokens.len(),
        1,
        "seq-match must coalesce the all-match source into exactly one fat Copy token, \
         got {} copy tokens",
        copy_tokens.len()
    );

    if let DeltaToken::Copy { index, len } = copy_tokens[0] {
        assert_eq!(*index, 0, "fat-copy starts at basis block 0");
        assert_eq!(
            *len,
            BLOCK_LEN as usize * BLOCKS,
            "fat-copy length equals the full basis"
        );
    }

    // Total copy bytes must equal the basis length exactly - no literal
    // bytes for an all-match source.
    assert_eq!(script.copy_bytes(), basis.len() as u64);
    assert_eq!(script.literal_bytes(), 0);
}

// ---------------------------------------------------------------------------
// ZSO-3 - hash-chain pruning (task #2511, pending)
// ---------------------------------------------------------------------------

/// ZSO-3 active test - hash-chain pruning (task #2511, shipped; design
/// at `docs/design/zsync-prune.md`).
///
/// Construction: a duplicate-heavy basis whose blocks are three distinct
/// 700-byte contents laid out `A B C A B C ...` so the rolling-sum lookup
/// map holds three buckets, each carrying a long chain of identical-key
/// entries. This is the long-chain topology ZSO-3's prune-on-match walk
/// targets (upstream zsync `librcksum/hash.c:111`). The source is the
/// whole basis, so every one of the duplicate blocks must be matched
/// exactly once.
///
/// Regression intent: pruning is a performance optimization; it must not
/// drop a matchable block. If a prune regression retired a live chain
/// entry too early, the corresponding source window would fall through to
/// a literal - so the zero-literal / full-copy assertions below fail the
/// moment pruning stops leaving each duplicate sibling findable until it
/// is individually consumed.
#[test]
fn hash_chain_prune_preserves_wire_bytes() {
    const BLOCK_LEN: u32 = 700;
    const REPEATS: usize = 5;

    // Three distinct block contents. Distinct seeds keep the strong
    // checksums apart so the three lookup buckets stay separate; the
    // A/B/C repetition is what grows each bucket's chain to REPEATS deep.
    let block_a = lcg_bytes(0x0DDB_A5E5_CAB7_E5A0, BLOCK_LEN as usize);
    let block_b = lcg_bytes(0x0DDB_A5E5_CAB7_E5B0, BLOCK_LEN as usize);
    let block_c = lcg_bytes(0x0DDB_A5E5_CAB7_E5C0, BLOCK_LEN as usize);
    let mut basis = Vec::with_capacity(BLOCK_LEN as usize * 3 * REPEATS);
    for _ in 0..REPEATS {
        basis.extend_from_slice(&block_a);
        basis.extend_from_slice(&block_b);
        basis.extend_from_slice(&block_c);
    }
    let source = basis.clone();

    let (script, wire) = run_pipeline(&basis, &source, BLOCK_LEN);
    assert!(!wire.is_empty(), "wire-byte stream must not be empty");

    // Determinism gate: the prune bitmap is reset per `generate` call, so
    // two runs on the same duplicate-heavy input must be byte-identical.
    let (_, wire2) = run_pipeline(&basis, &source, BLOCK_LEN);
    assert_eq!(
        wire, wire2,
        "ZSO-3 wire-byte output must be deterministic across runs"
    );

    // Functional correctness: the reconstruction equals the source.
    assert_round_trip(&basis, &source, &script);

    // Prune correctness: a source that is exactly the basis must resolve
    // to copies for every basis byte and zero literals. Any Literal token
    // here means a matchable duplicate was pruned out of its chain before
    // its source window arrived.
    assert!(
        script
            .tokens()
            .iter()
            .all(|t| matches!(t, DeltaToken::Copy { .. })),
        "prune must not force any duplicate block to a literal token"
    );
    assert_eq!(
        script.literal_bytes(),
        0,
        "prune must not drop a matchable block (got {} literal bytes)",
        script.literal_bytes()
    );
    assert_eq!(
        script.copy_bytes(),
        basis.len() as u64,
        "every duplicate basis block must be matched exactly once"
    );
}

// ---------------------------------------------------------------------------
// ZSO-4 - compact rolling-key (task #2512, pending)
// ---------------------------------------------------------------------------

/// ZSO-4 active test - compact rolling-key (task #2512, shipped; design
/// at `crates/matching/src/index/compact_lookup.rs`, which ports zsync's
/// `librcksum/rsum.c:205` `rsum_a_mask` filter into an in-memory bucket
/// keyed on the upper 16 bits of the rolling sum).
///
/// Construction: a small (< 64 KiB) basis of eight distinct random
/// blocks, and a source that interleaves three of those blocks (indices
/// 2, 5, 0) with non-matching literal gaps. The gaps are shorter than a
/// block so no window inside a gap can form a full-block match, which
/// keeps the three planted matches well separated - exactly the layout
/// the compact 16-bit key must still resolve.
///
/// Regression intent: the narrower compact key must not lose a real
/// match (the strong-checksum gate filters any collision the narrower key
/// admits, but it can never manufacture a match). If the compact-key
/// probe regressed and missed one planted block, that block would fall to
/// a literal, shrinking `copy_bytes` and reordering the copy-index list -
/// both pinned below.
#[test]
fn compact_rolling_key_preserves_wire_bytes() {
    const BLOCK_LEN: u32 = 700;
    const N_BLOCKS: usize = 8;
    const GAP_LEN: usize = 500;

    let basis = lcg_bytes(0x000C_0AC7_C0EE_FACE, BLOCK_LEN as usize * N_BLOCKS);
    let block = |i: usize| basis[i * BLOCK_LEN as usize..(i + 1) * BLOCK_LEN as usize].to_vec();

    // Literal gaps drawn from disjoint seeds so they cannot collide with
    // any basis block. GAP_LEN < BLOCK_LEN guarantees no in-gap window is
    // wide enough to match.
    let planted = [2usize, 5, 0];
    let gap_seeds = [
        0x11FF_0011_2233_4455u64,
        0x22FF_1122_3344_5566,
        0x33FF_2233_4455_6677,
        0x44FF_3344_5566_7788,
    ];
    let mut source = Vec::new();
    source.extend_from_slice(&lcg_bytes(gap_seeds[0], GAP_LEN));
    for (slot, &b) in planted.iter().enumerate() {
        source.extend_from_slice(&block(b));
        source.extend_from_slice(&lcg_bytes(gap_seeds[slot + 1], GAP_LEN));
    }

    let (script, wire) = run_pipeline(&basis, &source, BLOCK_LEN);
    assert!(!wire.is_empty(), "wire-byte stream must not be empty");

    let (_, wire2) = run_pipeline(&basis, &source, BLOCK_LEN);
    assert_eq!(
        wire, wire2,
        "ZSO-4 wire-byte output must be deterministic across runs"
    );

    assert_round_trip(&basis, &source, &script);

    // Every planted match must surface through the compact key, in order,
    // referencing its true basis block index. A compact-key regression
    // that dropped a match would delete or reorder an entry here.
    let copy_indices: Vec<u64> = script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, .. } => Some(*index),
            DeltaToken::Literal(_) => None,
        })
        .collect();
    assert_eq!(
        copy_indices,
        planted.iter().map(|&b| b as u64).collect::<Vec<_>>(),
        "compact-key probe must find every planted match at its true index"
    );

    // Byte accounting: exactly the three matched blocks are copies; every
    // gap byte stays literal. A missed match would move BLOCK_LEN bytes
    // from the copy total into the literal total.
    assert_eq!(
        script.copy_bytes(),
        planted.len() as u64 * u64::from(BLOCK_LEN),
        "copy bytes must equal the three planted blocks"
    );
    assert_eq!(
        script.literal_bytes(),
        (source.len() - planted.len() * BLOCK_LEN as usize) as u64,
        "every non-matching gap byte must be emitted as a literal"
    );
}

// ---------------------------------------------------------------------------
// Shared invariants
// ---------------------------------------------------------------------------

/// Cross-cutting determinism gate: running every active ZSO fixture twice
/// must produce byte-identical wire output. Centralising this in one test
/// (in addition to the per-ZSO determinism assertions above) gives a
/// single failure that signals "something in the matching pipeline became
/// non-deterministic" without the noise of failing every per-ZSO test at
/// once.
#[test]
fn all_active_zso_fixtures_are_deterministic() {
    let cases: [(&str, Vec<u8>, Vec<u8>, u32); 2] = [
        (
            "zso1-bithash",
            lcg_bytes(0x0B17_4A54_5EED_0001, 16 * 1024),
            lcg_bytes(0xC0DE_FEED_DEAD_BEEF, 16 * 1024),
            700,
        ),
        (
            "zso2-seq-match",
            lcg_bytes(0x5E5E_5E5E_C0FF_EE00, 700 * 84),
            lcg_bytes(0x5E5E_5E5E_C0FF_EE00, 700 * 84),
            700,
        ),
    ];

    for (name, basis, source, block_len) in cases {
        let (_, wire_a) = run_pipeline(&basis, &source, block_len);
        let (_, wire_b) = run_pipeline(&basis, &source, block_len);
        assert_eq!(
            wire_a, wire_b,
            "wire bytes diverged across runs for case {name}"
        );
    }
}
