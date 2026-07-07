//! Nextest port of upstream `testsuite/fuzzy.test` (local-copy legs).
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/fuzzy.test`.
//!
//! # Background
//!
//! `--fuzzy` lets the receiver reuse a same-content, differently-named file
//! already present in the destination directory as the delta basis for a new
//! transfer, instead of sending the whole file. Upstream's fuzzy.test seeds
//! `$todir/rsync2.c` with the exact bytes of `$fromdir/rsync.c`, then runs
//! `rsync -avvi --no-whole-file --fuzzy` and asserts the resulting tree
//! matches - which only holds if the basis was actually reused.
//!
//! # Why this matters (Rule 9)
//!
//! oc-rsync had a local-copy regression (#505, fixed in PR #6333 via the
//! engine tail-match) where `--fuzzy` failed to reuse a differently-named
//! same-content basis: the file was resent as pure literal data instead of
//! matching against the fuzzy basis. That defeats the entire point of the
//! option (bandwidth/IO savings) and silently diverges from upstream.
//!
//! These tests pin the fixed behaviour with a positive/negative control on
//! the `--stats` counters, which are the observable signal for "was the basis
//! reused":
//!
//! - **Positive** (`--fuzzy`): `Matched data` is the full file size and
//!   `Literal data` is 0 - the basis was reused, nothing was resent.
//! - **Negative** (no `--fuzzy`): `Literal data` is the full file size and
//!   `Matched data` is 0 - with no basis of the same name, the file is resent
//!   whole, proving the positive case's match came from the fuzzy basis and
//!   not from some unrelated quick-check shortcut.
//!
//! A regression that reintroduced #505 would flip the positive case back to
//! all-literal and fail the `Matched > 0` assertion.
//!
//! The scope is deliberately local-copy only. The #505 fix was local-copy
//! specific; the wire-path fuzzy still carries a pre-existing efficiency gap
//! (the last partial block is emitted as literal), so a wire-mode assertion on
//! exact byte counts would be nondeterministic. Local copy is the path the fix
//! landed on and the one this regression guard protects.
//!
//! # Upstream References
//!
//! - `testsuite/fuzzy.test` - the upstream script this file ports.
//! - `generator.c` - `find_fuzzy()` selects the fuzzy basis in the receiver's
//!   destination directory.

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

/// Extract the integer byte value from a `--stats` line like
/// `Matched data: 26,669 bytes`, stripping the thousands separators.
fn stat_bytes(stats: &str, label: &str) -> u64 {
    let line = stats
        .lines()
        .find(|l| l.trim_start().starts_with(label))
        .unwrap_or_else(|| panic!("stats output missing `{label}` line:\n{stats}"));
    let digits: String = line
        .chars()
        .skip_while(|c| *c != ':')
        .filter(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("could not parse `{label}` value from `{line}`"))
}

/// Seed a source file plus a differently-named destination file with the same
/// bytes, backdating the destination so quick-check cannot skip the transfer
/// on its own. Returns the source directory, destination directory, and the
/// file's size.
fn seed_fuzzy_pair(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, u64) {
    let from = root.join("from");
    let to = root.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    // A body large enough to span several blocks so a match is meaningful, and
    // deterministic so both control arms transfer the identical payload.
    let mut body = Vec::with_capacity(24_000);
    for i in 0..24_000u32 {
        body.push((i.wrapping_mul(2_654_435_761) >> 13) as u8);
    }
    let src = from.join("rsync.c");
    fs::write(&src, &body).expect("write source");

    // Differently-named basis with identical content in the destination dir.
    let basis = to.join("rsync2.c");
    fs::write(&basis, &body).expect("write fuzzy basis");
    // Backdate the basis so its mtime differs from the source; without a
    // same-name quick-check hit, only the fuzzy path can reuse it.
    let old = filetime::FileTime::from_unix_time(981_213_462, 0);
    filetime::set_file_mtime(&basis, old).expect("backdate basis");

    (from, to, body.len() as u64)
}

/// Positive control: `--fuzzy` reuses the differently-named same-content basis
/// so the whole file matches and nothing is resent as literal data.
///
/// This is the direct regression guard for #505 / PR #6333: before the fix the
/// file was resent whole (all literal), so `Matched data` would be 0 here.
// upstream: generator.c find_fuzzy() basis selection
#[test]
fn fuzzy_local_reuses_differently_named_basis() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (from, to, size) = seed_fuzzy_pair(root.path());

    let out = OcRsyncCliRunner::new()
        .args(["-a", "--no-whole-file", "--fuzzy", "--stats"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    let stats = out.stdout_str();
    let matched = stat_bytes(&stats, "Matched data:");
    let literal = stat_bytes(&stats, "Literal data:");

    assert_eq!(
        matched, size,
        "--fuzzy must match the entire file against the differently-named \
         basis (#505 regression guard); got Matched={matched}, expected {size}\n{stats}",
    );
    assert_eq!(
        literal, 0,
        "--fuzzy must resend nothing as literal when the basis is identical; \
         got Literal={literal}\n{stats}",
    );

    // The bytes must also be correct, not just the counters.
    let dst = to.join("rsync.c");
    assert_eq!(
        fs::read(&dst).expect("read dest"),
        fs::read(from.join("rsync.c")).expect("read src"),
        "fuzzy-reconstructed file must be byte-identical to the source",
    );
}

/// Negative control: without `--fuzzy` the same seed transfers the file whole
/// as literal data, proving the positive case's match came from the fuzzy
/// basis and not from an unrelated quick-check or same-name shortcut.
// upstream: generator.c - no fuzzy basis is consulted without --fuzzy
#[test]
fn without_fuzzy_local_resends_whole_file_as_literal() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let (from, to, size) = seed_fuzzy_pair(root.path());

    let out = OcRsyncCliRunner::new()
        .args(["-a", "--no-whole-file", "--stats"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    let stats = out.stdout_str();
    let matched = stat_bytes(&stats, "Matched data:");
    let literal = stat_bytes(&stats, "Literal data:");

    assert_eq!(
        literal, size,
        "without --fuzzy the file has no basis and is resent whole as literal; \
         got Literal={literal}, expected {size}\n{stats}",
    );
    assert_eq!(
        matched, 0,
        "without --fuzzy nothing may be matched against the unrelated basis; \
         got Matched={matched}\n{stats}",
    );
}
