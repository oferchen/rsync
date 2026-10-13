//! Golden `--version` capability text.
//!
//! The expected blocks were produced by re-running upstream 3.5.1's
//! `usage.c:189-235 print_info_flags()` wrap loop over oc's token list. That
//! loop reproduces the real 3.5.1 binary byte for byte when fed upstream's own
//! tokens, so these goldens pin upstream's token order, spelling and line
//! layout. oc's extra optimisation tokens follow upstream's last one in the
//! same style. Dropping, renaming or reordering any upstream token changes
//! the wrap and turns these tests red.

use super::*;

/// A configuration with every capability fixed, so the rendered text does not
/// depend on the host's runtime probes.
fn golden_config() -> VersionInfoConfig {
    VersionInfoConfig::builder()
        .supports_socketpairs(true)
        .supports_symlinks(true)
        .supports_symtimes(true)
        .supports_hardlinks(true)
        .supports_hardlink_specials(true)
        .supports_hardlink_symlinks(true)
        .supports_ipv6(true)
        .supports_atimes(true)
        .supports_batchfiles(true)
        .supports_inplace(true)
        .supports_append(true)
        .supports_acls(true)
        .supports_acl_at(false)
        .supports_xattrs(true)
        .secluded_args_mode(SecludedArgsMode::Optional)
        .supports_iconv(true)
        .supports_prealloc(true)
        .supports_stop_at(true)
        .supports_crtimes(false)
        .supports_idn(true)
        .supports_simd_roll(true)
        .supports_asm_roll(false)
        .supports_openssl_crypto(true)
        .supports_asm_md5(false)
        .supports_fast_allocator(true)
        .supports_copy_file_range(true)
        .supports_io_uring(true)
        .supports_parallel(true)
        .supports_mmap(true)
        .build()
}

/// The `Capabilities:` and `Optimizations:` blocks of a rendered report.
fn info_sections(rendered: &str) -> &str {
    let start = rendered
        .find("Capabilities:\n")
        .expect("report has a Capabilities section");
    let end = rendered[start..]
        .find("\nPlatform I/O:")
        .or_else(|| rendered[start..].find("\nio_uring:"))
        .expect("report has a section after Optimizations");
    &rendered[start..=start + end]
}

/// upstream: usage.c:110-125 - a `SUPPORT_ACL_FD` build (Linux with POSIX
/// ACLs) prints `ACL-at`/`no ACL-at` right after `ACLs`.
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
const GOLDEN_INFO_SECTIONS: &str = "\
Capabilities:
    64-bit files, 64-bit inums, 64-bit timestamps, 64-bit long ints,
    socketpairs, symlinks, symtimes, hardlinks, hardlink-specials,
    hardlink-symlinks, IPv6, atimes, batchfiles, inplace, append, ACLs,
    no ACL-at, xattrs, optional secluded-args, iconv, prealloc, stop-at,
    no crtimes, IDN
Optimizations:
    SIMD-roll, no asm-roll, openssl-crypto, no asm-MD5, jemalloc,
    copy-file-range, io-uring, parallel, mmap
";

/// Without `SUPPORT_ACL_FD` upstream prints no `ACL-at` token at all.
#[cfg(all(
    unix,
    not(target_os = "linux"),
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
const GOLDEN_INFO_SECTIONS: &str = "\
Capabilities:
    64-bit files, 64-bit inums, 64-bit timestamps, 64-bit long ints,
    socketpairs, symlinks, symtimes, hardlinks, hardlink-specials,
    hardlink-symlinks, IPv6, atimes, batchfiles, inplace, append, ACLs,
    xattrs, optional secluded-args, iconv, prealloc, stop-at, no crtimes, IDN
Optimizations:
    SIMD-roll, no asm-roll, openssl-crypto, no asm-MD5, jemalloc,
    copy-file-range, io-uring, parallel, mmap
";

#[cfg(all(
    unix,
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
#[test]
fn version_info_sections_match_upstream_layout() {
    let rendered = VersionInfoReport::new(golden_config()).human_readable();
    assert_eq!(info_sections(&rendered), GOLDEN_INFO_SECTIONS);
}

/// The JSON report (`-VV`) carries the same tokens; `ACL-at` becomes the
/// `ACL_at` key exactly as upstream's usage.c:192-193 rewrite produces it.
#[cfg(all(target_os = "linux", feature = "acl"))]
#[test]
fn version_json_reports_acl_at_on_linux() {
    let json = VersionInfoReport::new(golden_config()).machine_readable();
    assert!(json.contains("\"ACLs\": true,\n    \"ACL_at\": false,\n    \"xattrs\""));
    let pinned = golden_config().to_builder().supports_acl_at(true).build();
    let json = VersionInfoReport::new(pinned).machine_readable();
    assert!(json.contains("\"ACL_at\": true,"));
}

/// Without ACL support upstream's `SUPPORT_ACL_FD` is undefined, so the token
/// disappears instead of reading `no ACL-at`.
#[test]
fn acl_at_token_is_absent_without_acl_support() {
    let config = golden_config().to_builder().supports_acls(false).build();
    let rendered = VersionInfoReport::new(config).human_readable();
    assert!(!rendered.contains("ACL-at"));
}

/// The shipped default reflects the real ACL apply path, not a guess.
#[test]
fn default_acl_at_tracks_the_metadata_apply_path() {
    assert_eq!(
        VersionInfoConfig::new().supports_acl_at,
        ::metadata::ACL_APPLY_IS_FD_PINNED
    );
}

/// upstream: compat.c:477-478 - the version lists keep the num-0 `none` entry.
#[test]
fn checksum_list_matches_upstream() {
    let rendered = VersionInfoReport::new(golden_config()).human_readable();
    assert!(
        rendered.contains("Checksum list:\n    xxh128 xxh3 xxh64 (xxhash) md5 md4 sha1 none\n")
    );
    let json = VersionInfoReport::new(golden_config()).machine_readable();
    assert!(json.contains(
        "\"checksum_list\": [\n    \"xxh128\", \"xxh3\", \"xxh64\", \"md5\", \"md4\", \"sha1\", \"none\"\n  ]"
    ));
}

#[cfg(all(feature = "zstd", feature = "lz4"))]
#[test]
fn compress_list_matches_upstream() {
    let rendered = VersionInfoReport::new(golden_config()).human_readable();
    assert!(rendered.contains("Compress list:\n    zstd lz4 zlibx zlib none\n"));
}
