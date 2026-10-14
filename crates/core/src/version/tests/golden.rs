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
        .supports_acl_at(true)
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
#[cfg(all(
    unix,
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
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
    ACL-at, xattrs, optional secluded-args, iconv, prealloc, stop-at,
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

/// The `"capabilities"` and `"optimizations"` objects of a JSON report.
#[cfg(all(
    unix,
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
fn json_info_sections(json: &str) -> &str {
    let start = json
        .find(",\n  \"capabilities\"")
        .expect("JSON report has a capabilities object");
    let end = json
        .find(",\n  \"checksum_list\"")
        .expect("JSON report has a checksum_list after the info objects");
    &json[start..end]
}

/// upstream: usage.c:189-238 - the JSON (`-VV`) objects are packed by the
/// same 75-byte line buffer as the text report, `ACL-at` becomes `ACL_at`,
/// and each object closes with `  }`.
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
const GOLDEN_JSON_INFO_SECTIONS: &str = ",
  \"capabilities\": {
    \"file_bits\": 64, \"inum_bits\": 64, \"timestamp_bits\": 64,
    \"long_int_bits\": 64, \"socketpairs\": true, \"symlinks\": true,
    \"symtimes\": true, \"hardlinks\": true, \"hardlink_specials\": true,
    \"hardlink_symlinks\": true, \"IPv6\": true, \"atimes\": true,
    \"batchfiles\": true, \"inplace\": true, \"append\": true, \"ACLs\": true,
    \"ACL_at\": true, \"xattrs\": true, \"secluded_args\": \"optional\",
    \"iconv\": true, \"prealloc\": true, \"stop_at\": true, \"crtimes\": false,
    \"IDN\": true
  },
  \"optimizations\": {
    \"SIMD_roll\": true, \"asm_roll\": false, \"openssl_crypto\": true,
    \"asm_MD5\": false, \"jemalloc\": true, \"copy_file_range\": true,
    \"io_uring\": true, \"parallel\": true, \"mmap\": true
  }";

#[cfg(all(
    unix,
    not(target_os = "linux"),
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
const GOLDEN_JSON_INFO_SECTIONS: &str = ",
  \"capabilities\": {
    \"file_bits\": 64, \"inum_bits\": 64, \"timestamp_bits\": 64,
    \"long_int_bits\": 64, \"socketpairs\": true, \"symlinks\": true,
    \"symtimes\": true, \"hardlinks\": true, \"hardlink_specials\": true,
    \"hardlink_symlinks\": true, \"IPv6\": true, \"atimes\": true,
    \"batchfiles\": true, \"inplace\": true, \"append\": true, \"ACLs\": true,
    \"xattrs\": true, \"secluded_args\": \"optional\", \"iconv\": true,
    \"prealloc\": true, \"stop_at\": true, \"crtimes\": false, \"IDN\": true
  },
  \"optimizations\": {
    \"SIMD_roll\": true, \"asm_roll\": false, \"openssl_crypto\": true,
    \"asm_MD5\": false, \"jemalloc\": true, \"copy_file_range\": true,
    \"io_uring\": true, \"parallel\": true, \"mmap\": true
  }";

#[cfg(all(
    unix,
    target_pointer_width = "64",
    feature = "acl",
    feature = "xattr",
    feature = "iconv",
    feature = "idn"
))]
#[test]
fn version_json_sections_match_upstream_layout() {
    let json = VersionInfoReport::new(golden_config()).machine_readable();
    assert_eq!(json_info_sections(&json), GOLDEN_JSON_INFO_SECTIONS);
}

/// Without a pinned ACL apply path the JSON value is false, not just the text token.
#[cfg(all(target_os = "linux", feature = "acl"))]
#[test]
fn version_json_reports_acl_at_false_when_unpinned() {
    let unpinned = golden_config().to_builder().supports_acl_at(false).build();
    let json = VersionInfoReport::new(unpinned).machine_readable();
    assert!(json.contains("\"ACL_at\": false,"));
}

/// upstream: usage.c:336-339 - a blank line, then the warranty notice on
/// three lines with two spaces after each full stop and "Licence". Rendered
/// under upstream's program name it is byte-identical to the 3.5.1 binary.
#[test]
fn footer_matches_upstream_layout() {
    let rendered = VersionInfoReport::new(golden_config())
        .with_program_name("rsync")
        .human_readable();
    assert!(rendered.ends_with(
        "\n\nrsync comes with ABSOLUTELY NO WARRANTY.  This is free software, and you\n\
         are welcome to redistribute it under certain conditions.  See the GNU\n\
         General Public Licence for details.\n"
    ));
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

/// The Linux receiver writes ACLs through the confined leaf pin
/// (`metadata::acl_exacl::pinned`), so the shipped report says `ACL-at`
/// like a `SUPPORT_ACL_FD` upstream build on a kernel with the at-syscalls.
#[cfg(all(target_os = "linux", feature = "acl"))]
#[test]
fn shipped_linux_report_says_acl_at() {
    let rendered = VersionInfoReport::new(VersionInfoConfig::new()).human_readable();
    assert!(rendered.contains(" ACL-at,"), "{rendered}");
    assert!(!rendered.contains("no ACL-at"), "{rendered}");
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
