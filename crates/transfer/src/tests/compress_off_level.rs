//! Post-negotiation `init_compression_level()` off_level resolution.
//!
//! upstream: `token.c:76-98 init_compression_level()`, called from
//! `parse_compress_choice(1)` (`compat.c:820`) *after* `negotiate_the_strings()`
//! (`compat.c:809`). The meaning of `--compress-level=0` is codec-dependent and
//! therefore only knowable once the codec has been negotiated: zlib/zlibx have
//! `off_level == Z_NO_COMPRESSION` (`0`) so a literal `0` sets
//! `do_compression = CPRES_NONE`; zstd/lz4 have `off_level == CLVL_NOT_SPECIFIED`
//! and map a literal `0` to their default level, so they stay on. These tests
//! pin that WHY on [`crate::negotiated_level_disables_compression`], the shared
//! resolver the server transfer applies to the negotiated codec.

use protocol::CompressionAlgorithm;
use protocol::nstr::CLVL_NOT_SPECIFIED;

use crate::negotiated_level_disables_compression;

/// upstream: token.c:66 - zlib `off_level` is `Z_NO_COMPRESSION` (0), so
/// `--compress-level=0` against a negotiated zlib peer turns compression off
/// (`do_compression = CPRES_NONE`) and the sender frames plain tokens.
#[test]
fn zlib_level_zero_disables() {
    assert!(negotiated_level_disables_compression(
        false,
        0,
        CompressionAlgorithm::Zlib
    ));
}

/// upstream: token.c:63-66 - zlibx shares zlib's `off_level` of 0.
#[test]
fn zlibx_level_zero_disables() {
    assert!(negotiated_level_disables_compression(
        false,
        0,
        CompressionAlgorithm::ZlibX
    ));
}

/// upstream: token.c:75-78 - zstd `off_level` is `CLVL_NOT_SPECIFIED`, and a
/// literal `0` maps to `ZSTD_CLEVEL_DEFAULT`, so `--compress-level=0` against a
/// negotiated zstd peer keeps compression on (deflated data at the default
/// level), never simple tokens.
#[cfg(feature = "zstd")]
#[test]
fn zstd_level_zero_stays_on() {
    assert!(!negotiated_level_disables_compression(
        false,
        0,
        CompressionAlgorithm::Zstd
    ));
}

/// upstream: token.c:82-88 - lz4 forces min/max/def to 0 and never disables via
/// `--compress-level`.
#[cfg(feature = "lz4")]
#[test]
fn lz4_level_zero_stays_on() {
    assert!(!negotiated_level_disables_compression(
        false,
        0,
        CompressionAlgorithm::LZ4
    ));
}

/// A non-zero zlib level is inside `[min_level, max_level]`, never the
/// `off_level`, so it stays on. upstream: token.c:96-98 saturates instead of
/// disabling.
#[test]
fn zlib_nonzero_level_stays_on() {
    assert!(!negotiated_level_disables_compression(
        false,
        6,
        CompressionAlgorithm::Zlib
    ));
}

/// When `--compress-level` was not supplied the level is `CLVL_NOT_SPECIFIED`
/// and resolves to the codec default - never the off_level. upstream: token.c:93
/// substitutes `def_level` for the sentinel before the off_level check.
#[test]
fn unspecified_level_never_disables() {
    assert!(!negotiated_level_disables_compression(
        false,
        CLVL_NOT_SPECIFIED,
        CompressionAlgorithm::Zlib
    ));
}

/// An explicit `--compress-choice` already resolved the level against its known
/// codec at CLI-parse time, so the deferred resolver must leave that path
/// untouched even for zlib level 0. upstream: the explicit codec is resolved in
/// `parse_compress_choice` without deferral.
#[test]
fn explicit_choice_zlib_zero_not_redisabled_here() {
    assert!(!negotiated_level_disables_compression(
        true,
        0,
        CompressionAlgorithm::Zlib
    ));
}

/// A negotiated `none` codec has nothing to resolve; it is already off.
#[test]
fn negotiated_none_is_not_treated_as_a_level_disable() {
    assert!(!negotiated_level_disables_compression(
        false,
        0,
        CompressionAlgorithm::None
    ));
}
