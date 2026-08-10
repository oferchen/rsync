//! Hard regression fixture for the signed-byte rolling checksum.
//!
//! PR #3560 fixed a sign-extension bug in the rolling checksum where bytes
//! `>= 0x80` were treated as unsigned, diverging from upstream rsync's
//! `schar *buf = (schar *)buf1` cast (`checksum.c:285`). The proptest in
//! `rolling_simd_parity.rs` covers this via a self-contained reference, but
//! a corrupted reference would silently mask a regression. This file pins
//! down hand-computed expected `(s1, s2)` values for fixtures whose bytes
//! are dominated by `>= 0x80`. Any rolling-hash change that breaks signed
//! semantics will fail these tests even if the reference impl drifts.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/checksum.c:285` casts the
//!   buffer to `schar *`, so each byte contributes `(byte as i8) as i32`.
//! - `match.c:hash_search()` consumes the rolling digest as
//!   `(s2 << 16) | s1`, both terms masked to 16 bits.
//!
//! All fixtures below were computed offline by summing the signed
//! interpretations and applying `& 0xffff` only at the end. Sizes are chosen
//! to land on the SSE2/NEON 16-byte and AVX2 32-byte block boundaries plus
//! their +/- 1 neighbours.

use checksums::{RollingChecksum, RollingDigest};

/// Reference fixture: `[0x80, 0x81, 0x82, 0x83]` (4 bytes, all signed-negative).
///
/// As `i8`: `[-128, -127, -126, -125]`.
///
/// `s1 = -128 + -127 + -126 + -125 = -506`. `(-506) & 0xffff = 0xFE06`.
///
/// `s1` cumulative: `[-128, -255, -381, -506]`.
/// `s2 = -128 + -255 + -381 + -506 = -1270`. `(-1270) & 0xffff = 0xFB0A`.
#[test]
fn fixture_four_high_bytes_scalar_path() {
    let data = [0x80u8, 0x81, 0x82, 0x83];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    let digest = checksum.digest();
    assert_eq!(digest, RollingDigest::new(0xFE06, 0xFB0A, 4));
    assert_eq!(checksum.value(), 0xFB0A_FE06);
}

/// Reference fixture: 16 bytes all `0x80` (-128 as `i8`). Exercises the
/// SSE2/NEON 16-byte block boundary.
///
/// `s1 = -128 * 16 = -2048`. `(-2048) & 0xffff = 0xF800`.
///
/// `s2 = -128 * (16*17/2) = -128 * 136 = -17408`. `(-17408) & 0xffff = 0xBC00`.
#[test]
fn fixture_sixteen_bytes_0x80_sse2_neon_boundary() {
    let data = [0x80u8; 16];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    assert_eq!(checksum.digest(), RollingDigest::new(0xF800, 0xBC00, 16));
    assert_eq!(checksum.value(), 0xBC00_F800);
}

/// Reference fixture: 32 bytes all `0xFF` (-1 as `i8`). Exercises the
/// AVX2 32-byte block boundary.
///
/// `s1 = -1 * 32 = -32`. `(-32) & 0xffff = 0xFFE0`.
///
/// `s2 = -1 * (32*33/2) = -528`. `(-528) & 0xffff = 0xFDF0`.
#[test]
fn fixture_thirty_two_bytes_0xff_avx2_boundary() {
    let data = [0xFFu8; 32];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    assert_eq!(checksum.digest(), RollingDigest::new(0xFFE0, 0xFDF0, 32));
    assert_eq!(checksum.value(), 0xFDF0_FFE0);
}

/// Reference fixture: 33 bytes all `0xC0` (-64 as `i8`). Exercises AVX2
/// boundary + 1 to catch off-by-one in tail handling.
///
/// `s1 = -64 * 33 = -2112`. `(-2112) & 0xffff = 0xF7C0`.
///
/// `s2 = -64 * (33*34/2) = -64 * 561 = -35904`. `(-35904) & 0xffff = 0x73C0`.
#[test]
fn fixture_thirty_three_bytes_0xc0_avx2_boundary_plus_one() {
    let data = [0xC0u8; 33];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    assert_eq!(checksum.digest(), RollingDigest::new(0xF7C0, 0x73C0, 33));
    assert_eq!(checksum.value(), 0x73C0_F7C0);
}

/// Reference fixture: a single `0x80` byte. Smallest case where signed/unsigned
/// interpretations diverge: signed yields `-128`, unsigned would yield `+128`.
///
/// `s1 = -128`. `(-128) & 0xffff = 0xFF80`.
/// `s2 = -128`. `(-128) & 0xffff = 0xFF80`.
#[test]
fn fixture_single_byte_0x80() {
    let mut checksum = RollingChecksum::new();
    checksum.update(&[0x80u8]);

    assert_eq!(checksum.digest(), RollingDigest::new(0xFF80, 0xFF80, 1));
    assert_eq!(checksum.value(), 0xFF80_FF80);
}

/// Reference fixture for the rolling update path. Window length 4 over
/// `[0xFF, 0x80, 0x40, 0xC0, 0x20]` (signed: `[-1, -128, 64, -64, 32]`).
///
/// Initial window `[-1, -128, 64, -64]`:
///   `s1 = -1 + -128 + 64 + -64 = -129`. `(-129) & 0xffff = 0xFF7F`.
///   `s1` cumulative: `[-1, -129, -65, -129]`.
///   `s2 = -1 + -129 + -65 + -129 = -324`. `(-324) & 0xffff = 0xFEBC`.
///
/// After rolling (drop `-1`, add `32`), window is `[-128, 64, -64, 32]`:
///   `s1_new = (s1 - outgoing + incoming) = -129 - (-1) + 32 = -96`.
///   `(-96) & 0xffff = 0xFFA0`.
///   `s2_new = (s2 - n*outgoing + s1_new) = -324 - 4*(-1) + -96 = -416`.
///   `(-416) & 0xffff = 0xFE60`.
#[test]
fn fixture_rolling_update_signed_window() {
    let data = [0xFFu8, 0x80, 0x40, 0xC0, 0x20];
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[..4]);

    assert_eq!(rolling.digest(), RollingDigest::new(0xFF7F, 0xFEBC, 4));

    rolling
        .roll(0xFFu8, 0x20u8)
        .expect("roll on non-empty window must succeed");

    assert_eq!(rolling.digest(), RollingDigest::new(0xFFA0, 0xFE60, 4));

    let mut fresh = RollingChecksum::new();
    fresh.update(&data[1..5]);
    assert_eq!(rolling.digest(), fresh.digest());
}

/// Negative regression: explicitly verify that interpreting bytes as
/// **unsigned** would produce a *different* digest, so this test would fail
/// loud if the impl ever reverts to that interpretation.
///
/// For 16 bytes of `0x80`:
/// - signed: `s1 = -2048` -> `0xF800`, `s2 = -17408` -> `0xBC00`
/// - unsigned: `s1 = +2048` -> `0x0800`, `s2 = +17408` -> `0x4400`
#[test]
fn unsigned_interpretation_would_be_visibly_different() {
    let data = [0x80u8; 16];
    let mut checksum = RollingChecksum::new();
    checksum.update(&data);

    let actual = checksum.digest();
    let unsigned_alternative = RollingDigest::new(0x0800, 0x4400, 16);

    assert_ne!(
        actual, unsigned_alternative,
        "rolling checksum must use signed-byte interpretation; unsigned would yield {unsigned_alternative:?}"
    );
    assert_eq!(actual, RollingDigest::new(0xF800, 0xBC00, 16));
}
