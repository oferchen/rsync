//! Tests for the ZSO-3 consumed-block bitset on
//! [`super::DeltaSignatureIndex`].
//!
//! Pins the contracts for the interior-mutability hash-chain prune
//! described in `docs/design/zsync-prune.md`:
//!
//! - Marking a basis block flips the bit at the matching word and
//!   offset, observable through [`super::DeltaSignatureIndex::is_consumed`].
//! - A repeat lookup against a duplicate-content basis returns the next
//!   surviving sibling rather than the already-consumed block.
//! - Total emitted `Copy` tokens equal `min(source_dup, basis_dup)` for
//!   duplicate-heavy basis files, with each token naming a distinct
//!   basis block index.
//! - [`super::DeltaSignatureIndex::rebuild`] resets every bit so
//!   per-segment INC_RECURSE lifecycles (ZSO-7) start clean.
//! - Concurrent `mark_consumed` calls on distinct bits all converge
//!   under `Relaxed` atomics with no lost updates.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};
use std::sync::Arc;
use std::thread;

use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

use super::DeltaSignatureIndex;
use crate::generator::DeltaGenerator;
use crate::script::DeltaToken;

/// Fixed block length used by the prune tests. Small enough to keep
/// fixtures readable, large enough that the signature layout produces
/// at least two full-length blocks for the duplicate-bucket cases.
const TEST_BLOCK_LENGTH: u32 = 512;

/// Strong-checksum length (bytes). Matches the value used by the rest
/// of `crates/matching/src/index/` tests so a fixed signature layout
/// is in play.
const TEST_STRONG_LEN: u8 = 16;

/// Builds a [`DeltaSignatureIndex`] over the supplied basis bytes
/// using the fixed test block length. Helpers downstream rely on the
/// returned index having at least one full block (the smallest
/// duplicate-bucket case needs two).
fn build_index(basis: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Builds a basis composed of `n` byte-identical full-length blocks.
/// Used by the duplicate-bucket and chain-walk cases.
fn duplicate_basis(n: usize) -> Vec<u8> {
    let block_len = TEST_BLOCK_LENGTH as usize;
    let pattern: Vec<u8> = (0..block_len).map(|i| ((i * 7) % 251) as u8).collect();
    let mut basis = Vec::with_capacity(block_len * n);
    for _ in 0..n {
        basis.extend_from_slice(&pattern);
    }
    basis
}

/// Returns the number of `DeltaToken::Copy` tokens in `tokens`, the
/// distinct basis indices they reference, and the total bytes those
/// tokens cover. Used to assert the one-per-sibling invariant in the
/// duplicate-bucket cases.
fn copy_summary(tokens: &[DeltaToken]) -> (usize, Vec<u64>, u64) {
    let mut indices = Vec::new();
    let mut bytes = 0u64;
    let block_len = TEST_BLOCK_LENGTH as usize;
    for tok in tokens {
        if let DeltaToken::Copy { index, len } = tok {
            // Fat `Copy` tokens cover `run_len` adjacent basis blocks;
            // expand them so the assertion below sees one index per
            // basis block matched.
            let runs = (*len / block_len).max(1);
            for r in 0..runs {
                indices.push(index + r as u64);
            }
            bytes += *len as u64;
        }
    }
    (indices.len(), indices, bytes)
}

#[test]
fn prune_marks_block_consumed_after_match() {
    // Synthetic basis with two distinct full-length blocks plus a
    // source that exactly equals the basis. After running the
    // generator, every basis block that produced a `Copy` token must
    // report `is_consumed = true`.
    let block_len = TEST_BLOCK_LENGTH as usize;
    let basis: Vec<u8> = (0..block_len * 2).map(|i| ((i * 13) % 251) as u8).collect();
    let source = basis.clone();
    let index = build_index(&basis);
    let n_blocks = index.block_count();
    assert!(n_blocks >= 2, "fixture must produce at least two blocks");

    // Sanity: no bit is set before the generator runs.
    for i in 0..n_blocks {
        assert!(
            !index.is_consumed(i as u32),
            "bit {i} should be clear at start"
        );
    }

    let script = DeltaGenerator::new()
        .generate(Cursor::new(source), &index)
        .expect("script");

    let (_, copy_indices, _) = copy_summary(script.tokens());
    assert!(
        !copy_indices.is_empty(),
        "fixture should produce at least one Copy token"
    );
    for idx in &copy_indices {
        assert!(
            index.is_consumed(*idx as u32),
            "block {idx} should be marked consumed after Copy emission"
        );
    }
}

#[test]
fn prune_skips_consumed_block_on_repeat_lookup() {
    // Basis with two duplicate blocks (identical content -> identical
    // rsum -> bucket of length 2). The first lookup picks block 0;
    // after marking it consumed, the second lookup of the same rsum
    // must skip block 0 and return block 1 instead.
    let basis = duplicate_basis(2);
    let index = build_index(&basis);
    assert_eq!(
        index.block_count(),
        2,
        "fixture must produce exactly two duplicate blocks"
    );
    let block_len = index.block_length();
    let window = &basis[..block_len];
    let digest = index.block(0).rolling();

    let first = index.find_match_bytes(digest, window).expect("first match");
    assert_eq!(first, 0, "bucket walk picks block 0 in insertion order");

    index.mark_consumed(first as u32);

    let second = index
        .find_match_bytes(digest, window)
        .expect("second match should fall through to sibling");
    assert_eq!(
        second, 1,
        "consumed bit on block 0 must route the probe to block 1"
    );

    index.mark_consumed(second as u32);

    assert!(
        index.find_match_bytes(digest, window).is_none(),
        "with both siblings consumed, the probe must return None"
    );
}

#[test]
fn prune_preserves_match_correctness_under_duplicates() {
    // Basis with N duplicate blocks, source with the same N duplicate
    // blocks concatenated. With pruning on, the generator must emit
    // exactly N `Copy` token-equivalents (a fat Copy expands to one
    // wire op per basis block), each naming a distinct basis index.
    const N: usize = 4;
    let basis = duplicate_basis(N);
    let source = basis.clone();
    let index = build_index(&basis);
    assert_eq!(index.block_count(), N);

    let script = DeltaGenerator::new()
        .generate(Cursor::new(source.clone()), &index)
        .expect("script");

    let (copy_count, mut copy_indices, copy_bytes) = copy_summary(script.tokens());
    assert_eq!(
        copy_count, N,
        "expected exactly {N} Copy emissions for {N} duplicate source occurrences"
    );
    assert_eq!(
        copy_bytes,
        (N * TEST_BLOCK_LENGTH as usize) as u64,
        "matched-byte count must equal the full basis size"
    );

    copy_indices.sort_unstable();
    copy_indices.dedup();
    assert_eq!(
        copy_indices.len(),
        N,
        "every Copy must reference a distinct basis block index"
    );
}

#[test]
fn prune_state_resets_in_rebuild() {
    // Mark every basis block consumed, then rebuild from a different
    // signature. The reset contract from `rebuild` (ZSO-7 per-segment
    // lifecycle) must clear every bit so the new segment starts clean.
    let basis1: Vec<u8> = (0..2048).map(|i| ((i * 19) % 251) as u8).collect();
    let mut index = build_index(&basis1);
    let count1 = index.block_count();
    assert!(count1 >= 2, "fixture must produce at least two blocks");
    for i in 0..count1 {
        index.mark_consumed(i as u32);
        assert!(index.is_consumed(i as u32));
    }

    // Rebuild from a fresh basis of a different size and content so
    // the lookup table and block list both turn over.
    let basis2: Vec<u8> = (0..3000).map(|i| ((i * 7) % 251) as u8).collect();
    let params = SignatureLayoutParams::new(
        basis2.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let sig2 =
        generate_file_signature(basis2.as_slice(), layout, SignatureAlgorithm::Md4).expect("sig2");
    assert!(index.rebuild(&sig2, SignatureAlgorithm::Md4));

    let count2 = index.block_count();
    assert!(count2 >= 1);
    for i in 0..count2 {
        assert!(
            !index.is_consumed(i as u32),
            "bit {i} must be clear after rebuild"
        );
    }
}

#[test]
fn prune_state_resets_when_rebuild_keeps_block_count() {
    // Rebuilding with a signature whose block count fits in the same
    // number of words must still clear every bit; the
    // length-equal-words branch in `rebuild` is exercised here.
    let basis: Vec<u8> = (0..2048).map(|i| ((i * 11) % 251) as u8).collect();
    let mut index = build_index(&basis);
    let count = index.block_count();
    for i in 0..count {
        index.mark_consumed(i as u32);
    }

    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let sig =
        generate_file_signature(basis.as_slice(), layout, SignatureAlgorithm::Md4).expect("sig");
    assert!(index.rebuild(&sig, SignatureAlgorithm::Md4));

    for i in 0..index.block_count() {
        assert!(
            !index.is_consumed(i as u32),
            "bit {i} must be clear after equal-size rebuild"
        );
    }
}

#[test]
fn prune_is_thread_safe_under_concurrent_marks() {
    // N threads each mark a disjoint range of bits via `&self`. After
    // joining, every bit those threads claimed must be set; bits the
    // threads did not touch must remain clear. This pins the
    // `&self`-compatible `fetch_or` contract under load.
    const N_BLOCKS: usize = 256;
    let block_len = TEST_BLOCK_LENGTH as usize;
    let basis: Vec<u8> = (0..N_BLOCKS * block_len)
        .map(|i| (((i / block_len) * 23 + i % 251) % 251) as u8)
        .collect();
    let index = Arc::new(build_index(&basis));
    let block_count = index.block_count();
    let threads = 8;
    let chunk = block_count / threads;

    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let idx = Arc::clone(&index);
        let start = t * chunk;
        let end = if t + 1 == threads {
            block_count
        } else {
            (t + 1) * chunk
        };
        handles.push(thread::spawn(move || {
            for i in start..end {
                idx.mark_consumed(i as u32);
            }
        }));
    }
    for h in handles {
        h.join().expect("worker thread");
    }

    for i in 0..block_count {
        assert!(
            index.is_consumed(i as u32),
            "bit {i} should be set after concurrent marks"
        );
    }
    // Past-the-end indices stay false (and don't panic) regardless.
    assert!(!index.is_consumed(block_count as u32));
    assert!(!index.is_consumed(u32::MAX));
}

#[test]
fn mark_consumed_is_idempotent() {
    let basis: Vec<u8> = (0..2048).map(|i| ((i * 5) % 251) as u8).collect();
    let index = build_index(&basis);
    let count = index.block_count();
    assert!(count >= 1);
    index.mark_consumed(0);
    index.mark_consumed(0);
    index.mark_consumed(0);
    assert!(index.is_consumed(0));
}

#[test]
fn mark_consumed_ignores_out_of_range() {
    let basis: Vec<u8> = (0..2048).map(|i| ((i * 5) % 251) as u8).collect();
    let index = build_index(&basis);
    let count = index.block_count();
    // Out-of-range marks must not panic, and must not flip any
    // observable bit (the index has no slot for them).
    index.mark_consumed(count as u32);
    index.mark_consumed(u32::MAX);
    for i in 0..count {
        assert!(
            !index.is_consumed(i as u32),
            "no in-range bit should be set after out-of-range marks"
        );
    }
    assert!(!index.is_consumed(count as u32));
    assert!(!index.is_consumed(u32::MAX));
}

#[test]
fn clone_snapshots_consumed_bits_independently() {
    // Each clone must own its own consumed bitset: marking on the
    // original must not propagate to the clone, and vice versa. This
    // pins the per-session lifecycle contract from the parent design.
    let basis: Vec<u8> = (0..2048).map(|i| ((i * 17) % 251) as u8).collect();
    let index = build_index(&basis);
    let count = index.block_count();
    assert!(count >= 2);
    index.mark_consumed(0);

    let cloned = index.clone();
    assert!(cloned.is_consumed(0), "clone snapshots set bits");
    cloned.mark_consumed(1);

    assert!(
        !index.is_consumed(1),
        "original must not see the clone's later mark"
    );
}
