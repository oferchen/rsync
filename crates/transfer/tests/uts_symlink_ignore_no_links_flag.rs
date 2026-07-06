//! Port of the upstream rsync 3.4.4 testsuite `symlink-ignore.test`.
//!
//! Upstream source of truth:
//!   `target/interop/upstream-src/rsync-3.4.4/testsuite/symlink-ignore.test`
//!
//! Why this matters: rsync's default symlink policy is to *not* copy symlinks
//! at all. A symlink is only transferred when the user opts in with `-l`
//! (`--links`), `-L` (`--copy-links`), or `-a` (which implies `-l`). A recursive
//! copy with only `-r` must silently drop every symlink - dangling, relative,
//! and absolute alike - while still copying the regular files they point near.
//!
//! Regressing this is a real data-integrity hazard in both directions: copying
//! a symlink that upstream would have skipped can leak a path outside the tree
//! (a dangling or absolute link), and failing to copy a regular file because a
//! sibling symlink aborted the walk would silently lose data. The upstream test
//! guards exactly this: after `rsync -r from/ to`, the referent regular file is
//! present and none of the three symlink flavours survived.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use test_support::{OcRsyncCliRunner, create_tempdir, require_binary};

/// Build the same symlink fixture `rsync.fns`'s `build_symlinks` creates: a
/// real regular file `referent` plus three symlinks that must all be ignored.
fn build_symlinks(from: &Path) {
    fs::create_dir_all(from).expect("create from dir");
    fs::write(from.join("referent"), b"referent contents\n").expect("write referent");
    // Dangling: points at a name that does not exist.
    symlink("nonexistent-target", from.join("dangling")).expect("dangling symlink");
    // Relative: points at the sibling regular file.
    symlink("referent", from.join("relative")).expect("relative symlink");
    // Absolute: points at an absolute path outside the tree.
    symlink("/etc/hostname", from.join("absolute")).expect("absolute symlink");
}

/// A recursive copy without `-l`/`-L`/`-a` copies the regular file but drops
/// every symlink, matching upstream's default "symlinks should not be copied
/// at all" policy.
#[test]
fn recursive_copy_without_links_flag_drops_all_symlinks() {
    if !require_binary("oc-rsync") {
        return;
    }

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    build_symlinks(&from);

    // Trailing slash on the source: copy the *contents* of `from` into `to`,
    // exactly as the upstream test's `"$fromdir/" "$todir"` invocation does.
    let src = format!("{}/", from.display());
    let out = OcRsyncCliRunner::new()
        .arg("-r")
        .arg(&src)
        .arg(&to)
        .run()
        .expect("run oc-rsync -r");
    out.assert_success();

    // The regular file must arrive - dropping symlinks must not drop data.
    let referent = to.join("referent");
    assert!(
        referent.is_file(),
        "referent regular file must be copied under a plain -r transfer"
    );
    assert_eq!(
        fs::read(&referent).expect("read copied referent"),
        b"referent contents\n",
        "referent contents must round-trip byte-for-byte"
    );

    // None of the three symlink flavours may survive the default policy.
    for name in ["dangling", "relative", "absolute"] {
        let path = to.join(name);
        let meta = fs::symlink_metadata(&path);
        assert!(
            meta.is_err() || !meta.unwrap().file_type().is_symlink(),
            "{name} symlink must be ignored by a -r transfer without -l/-L/-a \
             (upstream default is to not copy symlinks at all)"
        );
    }

    // Guard against the "extra level of directories" bug the upstream test
    // also checks: `to/from` must not exist (the trailing slash means we copy
    // contents, not the `from` directory itself).
    assert!(
        !to.join("from").exists(),
        "trailing-slash source must copy contents, not nest an extra `from` dir"
    );
}
