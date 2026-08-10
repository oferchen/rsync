//! Hard regression fixture for rolling-checksum collisions.
//!
//! Constructs adversarial pairs of blocks that share an identical rolling
//! checksum (Adler-32-style with `CHAR_OFFSET = 0` and signed-byte
//! interpretation) but have different content. Verifies that:
//!
//! 1. The constructed pairs do collide on the rolling digest.
//! 2. Their strong checksums (MD4) genuinely differ.
//! 3. `DeltaSignatureIndex::find_match_bytes` rejects the colliding window
//!    via the strong-checksum gate.
//!
//! This test is the safety property that allows future probabilistic
//! prefilters (e.g. a bithash) to land: even if a prefilter mistakenly
//! claims "probably present" for a colliding rolling-checksum value, the
//! strong-checksum verify must catch the mismatch.
//!
//! # Collision construction
//!
//! For a block of length `n >= 4`, swapping the last four bytes from
//! `[a, b, c, d]` to `[d, c, b, a]` preserves both `s1` and `s2` iff
//! `3*(a - d) == c - b` (in plain integer arithmetic). Two concrete
//! solutions used here:
//!
//! - low-byte: `[0x01, 0x00, 0x03, 0x00]` / `[0x00, 0x03, 0x00, 0x01]`
//! - high-byte: `[0x80, 0x86, 0x80, 0x82]` / `[0x82, 0x80, 0x86, 0x80]`
//!
//! The high-byte case exercises the signed-byte path (`schar *buf` cast
//! at upstream `checksum.c:285`); without sign-extension the s1/s2 values
//! would diverge from upstream.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/checksum.c:285` - signed
//!   reinterpretation of buffer bytes for the rolling sum.
//! - `target/interop/upstream-src/rsync-3.4.1/match.c:140-345` -
//!   `hash_search()` consults strong checksum after a rolling-digest hit.

use checksums::RollingChecksum;
use matching::DeltaSignatureIndex;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::num::NonZeroU8;

const BLOCK_LEN: usize = 700;
const PREFIX_LEN: usize = BLOCK_LEN - 4;

/// Builds a single-block index from the given basis bytes using MD4 strong
/// checksums, mirroring the behaviour of the existing
/// `block_matching_accuracy.rs` integration tests.
fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Computes the rolling digest of `data` directly via `RollingChecksum`.
fn rolling_digest(data: &[u8]) -> checksums::RollingDigest {
    let mut rolling = RollingChecksum::new();
    rolling.update(data);
    rolling.digest()
}

/// Constructs a collision pair using `tail_a` / `tail_b` as the last
/// 4 bytes of a 700-byte block. The 696-byte prefix is shared between
/// the two blocks, so any rolling-checksum delta comes solely from the
/// tail. Caller-supplied tail values must satisfy
/// `tail_a[0] - tail_b[0] == 0` after the swap, encoded by
/// `tail_b == [tail_a[3], tail_a[2], tail_a[1], tail_a[0]]` only when
/// `3 * (tail_a[0] - tail_a[3]) == tail_a[2] - tail_a[1]`.
fn build_collision_pair(tail_a: [u8; 4], tail_b: [u8; 4]) -> (Vec<u8>, Vec<u8>) {
    let mut prefix = Vec::with_capacity(PREFIX_LEN);
    for i in 0..PREFIX_LEN {
        prefix.push((i.wrapping_mul(31).wrapping_add(7) % 256) as u8);
    }
    let mut block_a = prefix.clone();
    block_a.extend_from_slice(&tail_a);
    let mut block_b = prefix;
    block_b.extend_from_slice(&tail_b);
    assert_eq!(block_a.len(), BLOCK_LEN);
    assert_eq!(block_b.len(), BLOCK_LEN);
    (block_a, block_b)
}

/// Sanity-check the collision construction at the rolling-checksum level.
/// `[0x01, 0x00, 0x03, 0x00]` swap to `[0x00, 0x03, 0x00, 0x01]` preserves
/// `s1` and `s2` because `3 * (0x01 - 0x00) == 0x03 - 0x00`.
#[test]
fn low_byte_collision_pair_shares_rolling_digest() {
    let (block_a, block_b) =
        build_collision_pair([0x01, 0x00, 0x03, 0x00], [0x00, 0x03, 0x00, 0x01]);

    assert_ne!(block_a, block_b, "blocks must differ in content");
    assert_eq!(
        rolling_digest(&block_a),
        rolling_digest(&block_b),
        "constructed pair must share an identical rolling digest"
    );
}

/// High-byte case: bytes `>= 0x80` exercise the signed-`schar` interpretation
/// from upstream `checksum.c:285`. Without sign-extension the constructed
/// pair would produce divergent `s1`/`s2`, so this test is also a guard
/// against any future regression of the unsigned-vs-signed bug fixed in
/// PR #3560.
#[test]
fn high_byte_collision_pair_shares_rolling_digest() {
    let (block_a, block_b) =
        build_collision_pair([0x80, 0x86, 0x80, 0x82], [0x82, 0x80, 0x86, 0x80]);

    assert_ne!(block_a, block_b);
    assert_eq!(
        rolling_digest(&block_a),
        rolling_digest(&block_b),
        "high-byte collision pair must share rolling digest under signed interpretation"
    );
}

/// MD4 must distinguish the colliding pair. If this assertion ever fails
/// the entire delta correctness story collapses; this is the property
/// every probabilistic prefilter relies on.
#[test]
fn collision_pair_strong_checksums_differ() {
    let (block_a, block_b) =
        build_collision_pair([0x01, 0x00, 0x03, 0x00], [0x00, 0x03, 0x00, 0x01]);

    let index_a = build_index(&block_a);
    let index_b = build_index(&block_b);

    assert_ne!(
        index_a.block(0).strong(),
        index_b.block(0).strong(),
        "MD4 must distinguish blocks that collide on the rolling digest"
    );
}

/// `find_match_bytes` must reject a window whose rolling digest matches a
/// stored block but whose content is different. This is the bithash safety
/// property: even if a prefilter (current `tag_table` or any future
/// probabilistic structure) admits the rolling key, the strong-checksum
/// gate catches the mismatch.
#[test]
fn find_match_rejects_low_byte_collision_window() {
    let (block_a, block_b) =
        build_collision_pair([0x01, 0x00, 0x03, 0x00], [0x00, 0x03, 0x00, 0x01]);

    let index = build_index(&block_a);
    let probed_digest = rolling_digest(&block_b);

    assert_eq!(
        probed_digest,
        index.block(0).rolling(),
        "test relies on the constructed digest collision"
    );

    assert_eq!(
        index.find_match_bytes(probed_digest, &block_b),
        None,
        "colliding window with different content must not match via strong-checksum gate"
    );
}

/// Mirror of the low-byte collision-rejection check, this one stresses the
/// signed-byte (`>= 0x80`) path through the rolling-checksum and probe.
#[test]
fn find_match_rejects_high_byte_collision_window() {
    let (block_a, block_b) =
        build_collision_pair([0x80, 0x86, 0x80, 0x82], [0x82, 0x80, 0x86, 0x80]);

    let index = build_index(&block_a);
    let probed_digest = rolling_digest(&block_b);

    assert_eq!(probed_digest, index.block(0).rolling());

    assert_eq!(
        index.find_match_bytes(probed_digest, &block_b),
        None,
        "high-byte collision window must be rejected by strong-checksum gate"
    );
}

/// Self-match: probing with the original window must succeed. This is the
/// negative control - it proves the construction does not accidentally
/// break the success path.
#[test]
fn find_match_accepts_self_window() {
    let (block_a, _block_b) =
        build_collision_pair([0x01, 0x00, 0x03, 0x00], [0x00, 0x03, 0x00, 0x01]);

    let index = build_index(&block_a);
    let digest = index.block(0).rolling();

    assert_eq!(
        index.find_match_bytes(digest, &block_a),
        Some(0),
        "probe with the indexed block content must match block 0"
    );
}
