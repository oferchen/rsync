//! Nextest port of the upstream `testsuite/merge.test` scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/merge.test`.
//!
//! # Background
//!
//! Upstream's `merge.test` verifies that rsync can merge files from multiple
//! source directories (and bare file arguments) into a single destination in
//! one invocation. The canonical command is:
//!
//! ```sh
//! $RSYNC -avv deep/arg-test shallow from1/ from2/ from3/ to/
//! ```
//!
//! It exercises the multi-argument sender path: bare file arguments
//! (`deep/arg-test`, `shallow`), and three trailing-slash source directories
//! whose contents are unioned into `to/`. Overlapping names must resolve
//! last-writer-wins (`from1/one` == `from2/one` == `from3/one`), disjoint
//! names must all arrive, nested subdirectories (`sub1/`, `sub2/`) must merge
//! per-directory, and the awkward `dir-and-not-dir` collision - a directory in
//! `from1`, a plain file in `from3` - must resolve to the directory (the first
//! argument that reached it), never a type-confused hybrid.
//!
//! # Why this matters
//!
//! Merging multiple sources into one destination is a distinct code path from
//! a single-source recursive copy: the sender builds one flist spanning
//! several roots, and the generator/receiver must reconcile overlapping and
//! type-conflicting entries. A regression here silently drops or duplicates
//! files, or lets a plain-file argument clobber a directory of the same name.
//!
//! # What this test pins
//!
//! The transfer exits 0 and the destination tree is byte-for-byte the union
//! upstream `merge.test` builds in its `chk/` reference tree: every disjoint
//! file present with its own payload, every overlapping name carrying the
//! shared payload, nested `sub1`/`sub2` merged, and `dir-and-not-dir`
//! resolved to the directory form holding `inside`.
//!
//! # Upstream References
//!
//! - `testsuite/merge.test` - the upstream script this file ports.
//! - `flist.c` - multi-root file-list construction (`send_file_list`).
//! - `generator.c` - per-directory reconciliation of merged entries.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use test_support::{DirDiff, DirDiffOptions, OcRsyncCliRunner, require_binary};

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Build the three source directories plus the two bare-file arguments,
/// mirroring the upstream `merge.test` fixture block verbatim (dropping only
/// the never-referenced `from3/six` payload asymmetries - kept faithful here).
fn build_sources(root: &Path) {
    write(&root.join("from1/one"), "one\n");
    write(&root.join("from2/one"), "one\n");
    write(&root.join("from3/one"), "one\n");
    write(&root.join("from1/two"), "two\n");
    write(&root.join("from2/three"), "three\n");
    write(&root.join("from3/four"), "four\n");
    write(&root.join("from1/five"), "five\n");
    write(&root.join("from3/six"), "six\n");
    write(&root.join("from2/sub1/uno"), "sub1\n");
    write(&root.join("from3/sub1/uno"), "sub1\n");
    write(&root.join("from3/sub1/dos"), "sub2\n");
    write(&root.join("from2/sub1/tres"), "sub3\n");
    write(&root.join("from3/sub2/subby"), "subby\n");
    // dir-and-not-dir: a directory in from1 (with a file inside), a plain
    // file in from3. The directory argument reaches `to/` first and must win.
    write(&root.join("from1/dir-and-not-dir/inside"), "extra\n");
    write(&root.join("from3/dir-and-not-dir"), "not-dir\n");
    // Bare file arguments.
    write(&root.join("deep/arg-test"), "arg-test\n");
    write(&root.join("shallow"), "shallow\n");
}

/// Build the reference `chk/` tree upstream diffs `to/` against.
///
/// Upstream constructs it with a series of `cp_touch ... chk` calls; we write
/// the resulting union directly. `dir-and-not-dir` is the directory form (the
/// plain-file `from3` variant is shadowed).
fn build_expected(chk: &Path) {
    write(&chk.join("one"), "one\n");
    write(&chk.join("two"), "two\n");
    write(&chk.join("three"), "three\n");
    write(&chk.join("four"), "four\n");
    write(&chk.join("five"), "five\n");
    write(&chk.join("six"), "six\n");
    write(&chk.join("arg-test"), "arg-test\n");
    write(&chk.join("shallow"), "shallow\n");
    write(&chk.join("sub1/uno"), "sub1\n");
    write(&chk.join("sub1/dos"), "sub2\n");
    write(&chk.join("sub1/tres"), "sub3\n");
    write(&chk.join("sub2/subby"), "subby\n");
    write(&chk.join("dir-and-not-dir/inside"), "extra\n");
}

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(p: &Path) -> std::ffi::OsString {
    let mut s = p.as_os_str().to_os_string();
    s.push("/");
    s
}

#[test]
fn multiple_sources_merge_into_one_destination() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    build_sources(base);

    let to = base.join("to");
    let chk = base.join("chk");
    build_expected(&chk);

    // Upstream: $RSYNC -avv deep/arg-test shallow from1/ from2/ from3/ to/
    // Run with the source root as cwd so the bare relative arguments resolve,
    // exactly like the upstream `cd "$tmpdir"` preamble.
    let out = OcRsyncCliRunner::new()
        .cwd(base)
        .args([
            "-a",
            "deep/arg-test",
            "shallow",
            "from1/",
            "from2/",
            "from3/",
        ])
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    // Structural + content + mode comparison against the upstream chk tree.
    // mtimes are intentionally not compared: the sources are created at
    // slightly different wall-clock instants, and upstream itself normalizes
    // directory times before diffing rather than pinning them.
    match DirDiff::compare(&chk, &to, DirDiffOptions::structural()).expect("diff") {
        Ok(()) => {}
        Err(mismatch) => panic!("{}", mismatch.into_panic_message()),
    }
}
