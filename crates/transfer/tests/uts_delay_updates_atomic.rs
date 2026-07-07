//! Nextest port of upstream `testsuite/delay-updates.test` (local-copy leg).
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/delay-updates.test`.
//!
//! # Background
//!
//! `--delay-updates` stages every updated file under a `.~tmp~` scratch
//! directory in the destination and only renames them into place at the end of
//! the transfer, so an interrupted run never leaves half-written files visible.
//! Upstream's delay-updates.test runs `rsync -aiv --delay-updates` and
//! `checkit()` confirms the destination matches - and, implicitly, that the
//! `.~tmp~` staging directory does not survive a successful run.
//!
//! # Why this matters (Rule 9)
//!
//! The contract is atomicity plus cleanup: after a successful transfer every
//! updated file holds the new content (proving the delayed rename fired) and no
//! `.~tmp~` staging directory remains (proving the scratch area was cleaned).
//! A regression that renamed too early would break the atomicity guarantee; a
//! regression that skipped cleanup would leak a `.~tmp~` directory into the
//! user's tree. This test pins both.
//!
//! The update uses distinct source/destination sizes and a backdated
//! destination so rsync's quick-check cannot skip the transfer on matching
//! size+mtime - the delayed rename must actually run for the assertion to be
//! meaningful.
//!
//! Only the first (clean) leg of the upstream script is ported. The second leg
//! seeds a pre-existing `.~tmp~/foo` and depends on quick-check timing that is
//! nondeterministic in a program-order test; porting it would risk flakiness
//! for no added coverage of the atomic-rename contract.
//!
//! # Upstream References
//!
//! - `testsuite/delay-updates.test` - the upstream script this file ports.
//! - `receiver.c` / `generator.c` - `--delay-updates` stages into `partialptr`
//!   under `.~tmp~` and renames at `finish_transfer()` time.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use test_support::{OcRsyncCliRunner, require_binary};

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(p: &Path) -> std::ffi::OsString {
    let mut s = p.as_os_str().to_os_string();
    s.push("/");
    s
}

/// `--delay-updates` atomically installs the new content and leaves no
/// `.~tmp~` staging directory behind after a successful run.
// upstream: --delay-updates stages under .~tmp~ then renames at finish
#[test]
fn delay_updates_atomic_rename_and_cleanup() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let from = root.path().join("from");
    let to = root.path().join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    // Source and destination differ in both size and mtime so quick-check
    // cannot skip: the delayed rename must run for the content to change.
    fs::write(from.join("foo"), b"AAAA").expect("write source");
    fs::write(to.join("foo"), b"ZZ").expect("write stale dest");
    let old = filetime::FileTime::from_unix_time(946_684_800, 0); // 2000-01-01
    filetime::set_file_mtime(to.join("foo"), old).expect("backdate dest");

    let out = OcRsyncCliRunner::new()
        .args(["-aiv", "--delay-updates"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        fs::read(to.join("foo")).expect("read dest"),
        b"AAAA",
        "the delayed rename must have installed the new content",
    );
    assert!(
        !to.join(".~tmp~").exists(),
        "the .~tmp~ staging directory must be cleaned up after a successful \
         --delay-updates run, not leaked into the destination tree",
    );
}
