//! Nextest port of upstream `testsuite/atimes.test` (local-copy leg).
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/atimes.test`.
//!
//! # Background
//!
//! `-U` / `--atimes` tells rsync to preserve each file's access time in
//! addition to its modification time. Upstream's atimes.test backdates
//! `$fromdir/foo`'s atime to a fixed timestamp, runs `rsync -rtUgvvv`, and
//! `checkit()` confirms the destination tree matches with atimes compared.
//!
//! # Why this matters (Rule 9)
//!
//! Access-time preservation is an explicit metadata contract: a user who
//! passes `-U` expects the destination atime to equal the source atime, not
//! "now" (which is what a plain copy leaves, since reading the source to copy
//! it updates nothing but writing the dest sets a fresh atime). A regression
//! that dropped the atime restore would silently leave destination atimes at
//! transfer time, breaking backup/audit workflows that rely on atime.
//!
//! The test encodes the required behaviour, not merely current output: it sets
//! a distinct, fixed source atime and asserts the destination atime equals it
//! to the second. Restoring the wrong value (e.g. mtime, or leaving "now")
//! would fail the equality.
//!
//! # Upstream References
//!
//! - `testsuite/atimes.test` - the upstream script this file ports.
//! - `rsync.c` / `generator.c` - `--atimes` restores `st_atime` alongside
//!   `st_mtime` when the receiver commits the file.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use test_support::{OcRsyncCliRunner, require_binary};

/// Fixed source atime, matching the spirit of upstream's
/// `touch -a -t 200102031717.42` (2001-02-03 17:17:42 UTC = 981_213_462).
const SRC_ATIME_SECS: i64 = 981_213_462;

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(p: &Path) -> std::ffi::OsString {
    let mut s = p.as_os_str().to_os_string();
    s.push("/");
    s
}

/// Whole-second Unix atime of `path`.
fn atime_secs(path: &Path) -> i64 {
    let meta = fs::metadata(path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
    let at = meta.accessed().expect("atime");
    match at.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// `-U` preserves the source access time on the destination file.
///
/// The source atime is set to a fixed past value distinct from both its mtime
/// and the wall clock, so a pass proves the atime was actually carried over -
/// not left at transfer time and not confused with the mtime.
// upstream: --atimes restores st_atime on the receiver
#[test]
fn atimes_flag_preserves_source_access_time() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let from = root.path().join("from");
    let to = root.path().join("to");
    fs::create_dir_all(&from).expect("mkdir from");

    let src = from.join("foo");
    fs::write(&src, b"").expect("touch foo");

    // Set a fixed atime in the past and an mtime that differs from it, so the
    // restored atime cannot accidentally equal the mtime.
    let atime = filetime::FileTime::from_unix_time(SRC_ATIME_SECS, 0);
    let mtime = filetime::FileTime::from_unix_time(SRC_ATIME_SECS + 86_400, 0);
    filetime::set_file_times(&src, atime, mtime).expect("set source times");

    // Sanity: the source atime is the fixed value and differs from the wall
    // clock (so "now" would be an obviously-wrong result).
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    assert_eq!(atime_secs(&src), SRC_ATIME_SECS);
    assert!(
        now - SRC_ATIME_SECS > 86_400,
        "source atime must be clearly in the past"
    );

    let out = OcRsyncCliRunner::new()
        .args(["-rtUg"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    let dst = to.join("foo");
    assert!(dst.exists(), "destination file must exist");
    assert_eq!(
        atime_secs(&dst),
        SRC_ATIME_SECS,
        "-U must restore the source access time on the destination, not leave \
         it at transfer time or confuse it with the mtime",
    );
}
