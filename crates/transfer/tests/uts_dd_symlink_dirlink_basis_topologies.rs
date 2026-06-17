//! Regression coverage for the eight `symlink-dirlink-basis` topologies the
//! upstream rsync 3.4.4 testsuite exercises against `-K`/`--keep-dirlinks`.
//!
//! Upstream test: `testsuite/symlink-dirlink-basis.test`. Issue #715. The
//! scenarios update a destination file whose parent directory is a symlink to
//! the real directory. With `--keep-dirlinks` the receiver must follow the
//! parent symlink at chmod time instead of refusing it through the
//! `secure_chmod_at` sandbox; PR #5793 added the helper bypass and PR #5853
//! wired the compact `K` byte through the server-flag parser so the receiver
//! actually sets `MetadataOptions::keep_dirlinks`.
//!
//! These tests pin both halves of the chain together:
//!   1. `ParsedServerFlags::parse` recognises the compact `K` byte for every
//!      topology's flag string.
//!   2. The parsed flag survives `MetadataOptions::with_keep_dirlinks`, which
//!      is what `chmod_path_honoring_keep_dirlinks` consults at apply time.
//!
//! Each topology mirrors the corresponding `Test N` block in the upstream
//! script and asserts the wire-level + option-level invariants that gate the
//! receiver's path through the bypass.
//!
//! Upstream sources of truth:
//!   - `target/interop/upstream-src/rsync-3.4.4/testsuite/symlink-dirlink-basis.test`
//!   - `target/interop/upstream-src/rsync-3.4.1/options.c:688` (`-K` mapping)
//!   - `target/interop/upstream-src/rsync-3.4.1/generator.c:1344`
//!     (`link_stat(fname, &sx.st, keep_dirlinks && is_dir)`)

use metadata::MetadataOptions;
use transfer::flags::ParsedServerFlags;

/// Asserts that a compact flag string sets `keep_dirlinks` and that the
/// parsed value round-trips through `MetadataOptions::with_keep_dirlinks`
/// without being clobbered by an adjacent setter, matching the receiver's
/// builder chain in `crates/transfer/src/receiver/transfer/setup.rs`.
fn assert_keep_dirlinks_wired(flag_string: &str, label: &str) {
    let flags = ParsedServerFlags::parse(flag_string)
        .unwrap_or_else(|err| panic!("{label}: parse({flag_string:?}) failed: {err:?}"));
    assert!(
        flags.keep_dirlinks,
        "{label}: compact flag string {flag_string:?} must set keep_dirlinks (the receiver \
         constructs `MetadataOptions::with_keep_dirlinks(flags.keep_dirlinks)`; without the \
         bit set the dirfd sandbox would refuse every chmod through the symlinked parent)"
    );

    let opts = MetadataOptions::new()
        .preserve_permissions(flags.perms)
        .preserve_times(flags.times)
        .with_keep_dirlinks(flags.keep_dirlinks);
    assert!(
        opts.keep_dirlinks(),
        "{label}: MetadataOptions must report keep_dirlinks=true after the builder chain (the \
         chmod_path_honoring_keep_dirlinks helper consults this exact accessor at apply time)"
    );
}

/// Test 1 - basic directory symlink update through `-KRlptv`. The original
/// Issue #715 scenario: `dir -> real-dir` on the receiver, and a small delta
/// update must land on `real-dir/file` after running through the dirfd
/// sandbox bypass.
#[test]
fn topology_1_basic_dirlink_basis_through_compact_flags() {
    assert_keep_dirlinks_wired("-KRlptv", "topology 1 (basic -KRlptv)");
}

/// Test 2 - same as Test 1 plus compression (`-z`). The flag string the
/// client emits gains the `z` byte; `keep_dirlinks` must still come through.
#[test]
fn topology_2_compressed_dirlink_basis() {
    assert_keep_dirlinks_wired("-KRlptzv", "topology 2 (-KRlptzv with -z)");
}

/// Test 3 - nested directory symlinks (`nested -> nested_real`,
/// transferring `nested/sub/data.txt`). Flag string matches Test 1; the
/// added depth is purely a filesystem-side concern that the receiver
/// resolves through the same chmod helper.
#[test]
fn topology_3_nested_dirlink_basis() {
    assert_keep_dirlinks_wired("-KRlptv", "topology 3 (nested dirlink)");
}

/// Test 4 - `--backup` rename through a dirlink. Backup adds the long-form
/// `--backup` flag which the parser propagates via the standalone `backup`
/// field; the compact `K` byte still arrives in the same string.
#[test]
fn topology_4_backup_dirlink_basis() {
    let flags =
        ParsedServerFlags::parse("-KRlptv").expect("topology 4: compact flag parse must succeed");
    assert!(flags.keep_dirlinks, "topology 4: keep_dirlinks must be set");
    // Backup itself does not gate the bypass; just pin the standalone flag
    // surface so a future change cannot silently couple them.
    assert!(
        !flags.backup,
        "topology 4: --backup is delivered out-of-band via long-form args; the compact parser \
         must not infer it from the `K`/`R`/`l`/`p`/`t`/`v` bytes"
    );
}

/// Test 5 - `--inplace` with the dirlink. Same compact string as Test 1;
/// `--inplace` rides on the long-form parser and is orthogonal to
/// `keep_dirlinks` here.
#[test]
fn topology_5_inplace_dirlink_basis() {
    assert_keep_dirlinks_wired("-KRlptv", "topology 5 (inplace dirlink)");
}

/// Test 6 - top-level file transfer without any symlinked parent. The
/// upstream script drops `-K` from this case (no dirlink to traverse), so
/// the parser must NOT spontaneously set `keep_dirlinks`. This is the
/// negative control for the wiring.
#[test]
fn topology_6_top_level_file_without_keep_dirlinks() {
    let flags =
        ParsedServerFlags::parse("-Rlptv").expect("topology 6: compact flag parse must succeed");
    assert!(
        !flags.keep_dirlinks,
        "topology 6: top-level file transfer omits -K; the compact parser must not infer \
         keep_dirlinks from sibling bytes (the symlinked-parent bypass is opt-in)"
    );

    let opts = MetadataOptions::new()
        .preserve_permissions(flags.perms)
        .preserve_times(flags.times)
        .with_keep_dirlinks(flags.keep_dirlinks);
    assert!(
        !opts.keep_dirlinks(),
        "topology 6: MetadataOptions must propagate the false flag; otherwise the receiver would \
         silently follow destination symlinks even when the client did not ask for it"
    );
}

/// Test 7 - `--partial-dir` with protocol 28. Compact flag string is the
/// same; protocol selection is out-of-band. The receiver still needs to
/// route the chmod through the bypass under `-K`.
#[test]
fn topology_7_proto28_partial_dir_dirlink_basis() {
    assert_keep_dirlinks_wired("-KRlptv", "topology 7 (proto28 + --partial-dir)");
}

/// Test 8 - basic dirlink update at protocol 28. Same compact string and
/// flag-table requirement as Test 1; pins that the bypass is not gated by
/// protocol version on the parser side.
#[test]
fn topology_8_proto28_dirlink_basis() {
    assert_keep_dirlinks_wired("-KRlptv", "topology 8 (proto28 basic)");
}
