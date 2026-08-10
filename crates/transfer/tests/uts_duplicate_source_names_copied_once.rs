//! Port of the upstream rsync 3.4.4 testsuite `duplicates.test`.
//!
//! Upstream source of truth:
//!   `target/interop/upstream-src/rsync-3.4.4/testsuite/duplicates.test`
//!   `flist.c` `clean_flist()` - the dedup pass that collapses repeated names.
//!
//! Why this matters: a user can name the same source more than once on the
//! command line (shell variable expansion, overlapping wildcards). If the file
//! list kept both copies, rsync could try to update the first copy while
//! generating checksums for the second at the same time - a genuine race that
//! upstream fixes by de-duplicating the flist in `clean_flist()`. The upstream
//! test lists the same directory ten times and asserts each contained file is
//! copied *exactly once* (the `grep -c '^name1$' == 1` check), not ten times.
//!
//! We assert the observable invariant two ways so the test fails if dedup
//! regresses in either the wire path or the output path:
//!   1. Each source file appears exactly once in the destination tree (a
//!      duplicated flist would still produce one file on disk, but a broken
//!      dedup that mangled names could drop or misplace it).
//!   2. With `-vv` itemized output, each transferred name is listed exactly
//!      once - mirroring upstream's `grep -c` assertion that catches a flist
//!      that carried the same entry N times.

#![cfg(unix)]

use std::fs;

use test_support::{OcRsyncCliRunner, create_tempdir, require_binary};

/// The same source directory repeated many times on the command line must
/// still copy each contained file exactly once - both on disk and in the
/// verbose transfer log.
#[test]
fn repeated_source_directory_copies_each_file_exactly_once() {
    if !require_binary("oc-rsync") {
        return;
    }

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    fs::create_dir_all(&from).expect("create from dir");
    fs::create_dir_all(&to).expect("create to dir");
    fs::write(from.join("name1"), b"This is the file\n").expect("write name1");
    fs::write(from.join("name2"), b"second file\n").expect("write name2");

    // Repeat the trailing-slash source ten times, exactly like upstream's
    // `'$fromdir/' '$fromdir/' ... '$todir/'` invocation.
    let src = format!("{}/", from.display());
    let dst = format!("{}/", to.display());
    let mut runner = OcRsyncCliRunner::new().arg("-rvv");
    for _ in 0..10 {
        runner = runner.arg(&src);
    }
    let out = runner.arg(&dst).run().expect("run oc-rsync -rvv x10");
    out.assert_success();

    // Each file must exist once with the right bytes; a broken dedup that
    // dropped a name would fail these reads.
    assert_eq!(
        fs::read(to.join("name1")).expect("read name1"),
        b"This is the file\n",
        "name1 must be copied once with intact contents"
    );
    assert_eq!(
        fs::read(to.join("name2")).expect("read name2"),
        b"second file\n",
        "name2 must be copied once with intact contents"
    );

    // Upstream's core assertion: each name appears exactly once in the
    // transfer log even though the source was listed ten times. A flist that
    // failed to dedup would itemize the same name repeatedly.
    let log = out.stdout_str();
    let count_lines = |needle: &str| log.lines().filter(|l| l.trim() == needle).count();
    assert_eq!(
        count_lines("name1"),
        1,
        "name1 must be transferred exactly once despite 10 duplicate sources\n\
         --- oc-rsync -rvv stdout ---\n{log}"
    );
    assert_eq!(
        count_lines("name2"),
        1,
        "name2 must be transferred exactly once despite 10 duplicate sources\n\
         --- oc-rsync -rvv stdout ---\n{log}"
    );
}
