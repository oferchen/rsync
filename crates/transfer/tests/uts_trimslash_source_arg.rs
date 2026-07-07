//! Nextest port of the upstream `testsuite/trimslash.test` scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/trimslash.test`.
//!
//! # Background
//!
//! Upstream's `trimslash.test` exercises rsync's tiny trailing-slash trimmer
//! (upstream `util1.c:trim_trailing_slashes`) via a standalone `trimslash`
//! helper. The rule it enforces: any run of trailing slashes on a path
//! collapses to the semantics of a single trailing slash, except that a path
//! of pure slashes (`//`, `////`) trims down to a single `/`.
//!
//! For rsync itself, this trimming is what makes a source argument's trailing
//! slash meaningful: `src/` and `src///` both mean "copy the *contents* of
//! `src` into the destination", while `src` (no slash) means "copy the `src`
//! directory itself into the destination". This test ports the trimmer's
//! observable contract into the transfer layer, where it actually matters,
//! rather than shelling out to a private helper binary that oc-rsync does not
//! ship.
//!
//! # Why this matters
//!
//! The trailing-slash distinction is one of rsync's most notorious usability
//! cliffs: getting it wrong nests a directory one level too deep or flattens a
//! copy that should have been nested. A regression in trailing-slash trimming
//! silently changes destination layout for a huge fraction of real invocations.
//! Because multiple trailing slashes must be treated identically to one, a
//! naive "strip exactly one slash" implementation is wrong - this test guards
//! against exactly that.
//!
//! # What this test pins
//!
//! - `src/` copies the contents of `src` (dest gains `sub/`), not `src` itself.
//! - `src///` behaves identically to `src/` (multiple slashes collapse).
//! - `src` (no trailing slash) copies the directory itself (dest gains
//!   `src/sub/`).
//!
//! # Upstream References
//!
//! - `testsuite/trimslash.test` - the upstream script this file ports.
//! - `util1.c` - `trim_trailing_slashes()`, the function under test.
//! - `flist.c` - trailing-slash handling of source args (`send_file_list`).

#![cfg(unix)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use test_support::{OcRsyncCliRunner, require_binary};

/// Build a `src` dir containing a single nested file, so the contents-vs-dir
/// distinction is observable purely from which directory names appear in dest.
fn build_src(base: &Path) -> std::path::PathBuf {
    let src = base.join("src");
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::write(src.join("sub/file"), "payload\n").expect("write nested file");
    src
}

/// Run a copy of `source_arg` into a fresh `dest`, asserting exit 0, and
/// return the dest path.
fn copy_into(base: &Path, dest_name: &str, source_arg: std::ffi::OsString) -> std::path::PathBuf {
    let dest = base.join(dest_name);
    fs::create_dir_all(&dest).expect("mkdir dest");
    let mut dest_arg = dest.as_os_str().to_os_string();
    dest_arg.push("/");
    let out = OcRsyncCliRunner::new()
        .arg("-a")
        .arg(source_arg)
        .arg(dest_arg)
        .run()
        .expect("run oc-rsync");
    out.assert_success();
    dest
}

/// Append `n` slashes to `path`'s string form.
fn with_slashes(path: &Path, n: usize) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    for _ in 0..n {
        s.push("/");
    }
    s
}

#[test]
fn trailing_slashes_collapse_to_single_slash_semantics() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let src = build_src(base);

    // `src/` copies contents: dest gains `sub/`, not `src/`.
    let d1 = copy_into(base, "dst_one_slash", with_slashes(&src, 1));
    assert!(
        d1.join("sub/file").is_file(),
        "single trailing slash must copy contents"
    );
    assert!(
        !d1.join("src").exists(),
        "single trailing slash must not nest the src dir itself"
    );

    // `src///` must behave identically to `src/` (collapse of multiple slashes).
    let d3 = copy_into(base, "dst_triple_slash", with_slashes(&src, 3));
    assert!(
        d3.join("sub/file").is_file(),
        "multiple trailing slashes must collapse to contents-copy"
    );
    assert!(
        !d3.join("src").exists(),
        "multiple trailing slashes must not nest the src dir itself"
    );

    // `src` (no slash) copies the directory itself: dest gains `src/sub/`.
    let d0 = copy_into(base, "dst_no_slash", src.as_os_str().to_os_string());
    assert!(
        d0.join("src/sub/file").is_file(),
        "no trailing slash must nest the src directory itself"
    );
    assert!(
        !d0.join("sub").exists(),
        "no trailing slash must not copy contents flat"
    );
}
