//! Nextest port of the upstream `testsuite/mkpath.test` scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/mkpath.test`.
//!
//! # Background
//!
//! Upstream's `mkpath.test` verifies the `--mkpath` option, which tells rsync
//! to create any missing leading components of the destination path before
//! transferring. Without `--mkpath`, rsync only creates the final destination
//! directory (or none at all for a single-file copy); a multi-level missing
//! prefix like `to/foo/bar/baz/down/deep/` is an error. With `--mkpath` the
//! whole prefix is materialized.
//!
//! The upstream script drives several distinct shapes through the flag:
//!
//! ```sh
//! $RSYNC -aiv --mkpath from/text $deep_dir/new     # single file -> new leaf name
//! $RSYNC -aiv --mkpath from/text $deep_dir/         # single file -> into a new dir
//! $RSYNC -aiv --mkpath from/text to_text            # no missing path at all
//! ```
//!
//! # Why this matters
//!
//! `--mkpath` changes destination-path resolution: the difference between
//! "create only the final dir" and "create the entire missing prefix" is a
//! distinct code path with sharp edges. The trailing-slash distinction
//! (`.../deep/new` names a file leaf, `.../deep/` names a directory the source
//! file lands inside) must be honored while simultaneously fabricating the
//! prefix. A regression here either refuses valid `--mkpath` copies or, worse,
//! silently mis-parents the file (writing `deep` as a file instead of a
//! directory, or dropping the leaf rename).
//!
//! # What this test pins
//!
//! For each upstream shape, the transfer exits 0 and the expected file lands at
//! exactly the expected path with the source contents, with the full
//! previously-missing directory prefix created.
//!
//! # Upstream References
//!
//! - `testsuite/mkpath.test` - the upstream script this file ports.
//! - `options.c` - `--mkpath` option wiring (`mkpath_dest_arg`).
//! - `generator.c` - destination-prefix directory materialization.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use test_support::{OcRsyncCliRunner, require_binary};

/// Trailing-slash form of a path (as an `OsString` argument).
fn slash(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push("/");
    s
}

/// Run oc-rsync from `cwd` with `-a --mkpath` plus `args`, asserting exit 0.
fn run_mkpath<I, S>(cwd: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = OcRsyncCliRunner::new()
        .cwd(cwd)
        .arg("-a")
        .arg("--mkpath")
        .args(args)
        .run()
        .expect("run oc-rsync");
    out.assert_success();
}

#[test]
fn mkpath_creates_missing_destination_prefix() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();

    // Source file, mirroring upstream's `from/text`.
    let from = base.join("from");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::write(from.join("text"), "mkpath payload\n").expect("write source");

    // Upstream: deep_dir=to/foo/bar/baz/down/deep
    let deep_rel = "to/foo/bar/baz/down/deep";

    // Leg 1: single file -> a new leaf name several missing levels down.
    // $RSYNC -aiv --mkpath from/text $deep_dir/new
    run_mkpath(base, ["from/text", &format!("{deep_rel}/new")]);
    let leaf = base.join(format!("{deep_rel}/new"));
    assert!(
        leaf.is_file(),
        "leaf file not created at {}",
        leaf.display()
    );
    assert_eq!(fs::read_to_string(&leaf).unwrap(), "mkpath payload\n");
    fs::remove_dir_all(base.join("to")).expect("cleanup to");

    // Leg 2: single file -> into a brand-new directory (trailing slash).
    // $RSYNC -aiv --mkpath from/text $deep_dir/
    let deep_dir = base.join(deep_rel);
    run_mkpath(base, ["from/text".into(), slash(&deep_dir)]);
    let into_dir = deep_dir.join("text");
    assert!(
        into_dir.is_file(),
        "file not placed inside new dir at {}",
        into_dir.display()
    );
    assert_eq!(fs::read_to_string(&into_dir).unwrap(), "mkpath payload\n");
    fs::remove_dir_all(base.join("to")).expect("cleanup to");

    // Leg 3: no missing path at all - a plain leaf in the cwd.
    // $RSYNC -aiv --mkpath from/text to_text
    run_mkpath(base, ["from/text", "to_text"]);
    let plain = base.join("to_text");
    assert!(
        plain.is_file(),
        "plain leaf not created at {}",
        plain.display()
    );
    assert_eq!(fs::read_to_string(&plain).unwrap(), "mkpath payload\n");
}
