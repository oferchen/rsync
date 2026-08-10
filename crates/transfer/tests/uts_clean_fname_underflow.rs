//! Nextest port of upstream `testsuite/clean-fname-underflow.test`.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/clean-fname-underflow.test`.
//!
//! # Background
//!
//! Upstream's `clean_fname()` collapses `.` and `..` path components in place.
//! A crafted filter path such as `a/../test` walks the write pointer backwards
//! when it eats the `..`; a boundary bug there can underflow the buffer and
//! read or write before the start of the name, crashing the process. The
//! upstream test drives the sender with `--filter='merge a/../test'` and
//! asserts rsync does not die from a signal - a non-zero exit for the bogus
//! input is fine, a SIGSEGV/SIGABRT is not.
//!
//! # Why this matters (Rule 9)
//!
//! This is a memory-safety / crash-hardening guard. `clean_fname()` runs on
//! attacker-influenceable path strings (filter files, remote file lists), so a
//! buffer underflow here is a security bug, not a cosmetic one. The test
//! encodes the invariant "path cleaning never crashes on adversarial `../`
//! sequences": it must terminate cleanly with an ordinary exit code and never
//! be killed by a signal. A regression that reintroduced an underflow would
//! trip `assert_no_signal_death`.
//!
//! oc-rsync's path cleaning is memory-safe by construction (no raw pointer
//! arithmetic), so this port is the positive proof that the safe rewrite still
//! rejects the crafted input gracefully rather than, say, panicking - a Rust
//! panic in a server child surfaces as a signal death (SIGABRT) and would be
//! caught here.
//!
//! # Upstream References
//!
//! - `testsuite/clean-fname-underflow.test` - the upstream script this file
//!   ports.
//! - `util1.c` - `clean_fname()` in-place `.`/`..` collapsing.

#![cfg(unix)]

use std::fs;

use test_support::{OcRsyncCliRunner, require_binary};

/// The server-sender invocation with the crafted `merge a/../test` filter must
/// terminate cleanly - any exit code is acceptable, a signal death is not.
///
/// Mirrors the upstream shell test's `if $rsync_bin --server --sender ...` arm:
/// a non-zero status is expected for bogus input, but `status >= 128`
/// (signal-killed) is a failure. Here we assert `assert_no_signal_death`, which
/// is the direct translation of that guard.
// upstream: util1.c clean_fname() underflow hardening
#[test]
fn clean_fname_handles_dotdot_without_crashing() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let workdir = root.path().join("workdir");
    fs::create_dir_all(workdir.join("mod")).expect("mkdir workdir/mod");

    let out = OcRsyncCliRunner::new()
        .cwd(&workdir)
        .args([
            "--server",
            "--sender",
            "-vlr",
            "--filter=merge a/../test",
            ".",
            "mod/",
        ])
        .run()
        .expect("run oc-rsync server sender");

    // The crafted path must not crash the process. A non-zero exit for the
    // bogus filter is fine; a signal (SIGSEGV/SIGABRT) is the regression this
    // guard rejects.
    out.assert_no_signal_death();
}
