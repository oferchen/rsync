//! Client stderr escapes control bytes as rsync 3.5.2 does.
//!
//! upstream: log.c:499 `rwrite()` sends every FERROR line through
//! `filtered_fwrite()` - for a client without `-8` after the `ic_chck` check of
//! log.c:463-496 - which writes C0 (tab excepted), DEL and the C1 controls as
//! `\#ooo` (log.c:276-293). A refused `--filter` rule echoes operator text, so
//! a VT in it prints as `\#013` both in the `'%c'` detail and in the echoed
//! rule (exclude.c:1376).
//!
//! # Gating
//!
//! Each cell runs oc-rsync and upstream 3.5.2 with the same arguments and
//! compares the cause line byte for byte, plus the exit code. It self-skips
//! unless `OC_RSYNC_UPSTREAM_COMPAT=1` and an upstream 3.5.2 binary resolves
//! (override with `OC_RSYNC_UPSTREAM_BIN_3_5_2`); with the variable set, a
//! missing oracle fails the cell.
#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// The exit code and the first stderr line of one run. The `log_exit()`
/// trailer that follows names the program and a source location, which
/// legitimately differ, so it is left out.
#[derive(Debug, PartialEq, Eq)]
struct Refusal {
    exit: Option<i32>,
    cause: Vec<u8>,
}

fn refusal(mut command: Command, cwd: &Path, filter: &[u8], extra: &[&str]) -> Refusal {
    let mut arg = b"--filter=".to_vec();
    arg.extend_from_slice(filter);
    let output = command
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .args(extra)
        .arg("-r")
        .arg(OsStr::from_bytes(&arg))
        .args(["src/", "dst/"])
        .output()
        .expect("spawn rsync");
    let cause = output
        .stderr
        .split(|&byte| byte == b'\n')
        .next()
        .unwrap_or_default()
        .to_vec();
    Refusal {
        exit: output.status.code(),
        cause,
    }
}

fn assert_refusal_matches_upstream(filter: &[u8], extra: &[&str]) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let tmp = test_support::create_tempdir();
    fs::create_dir_all(tmp.path().join("src")).expect("mkdir src");
    fs::create_dir_all(tmp.path().join("dst")).expect("mkdir dst");
    let expected = refusal(upstream.command(), tmp.path(), filter, extra);
    let actual = refusal(Command::new(oc_rsync_bin()), tmp.path(), filter, extra);
    assert_eq!(
        String::from_utf8_lossy(&actual.cause),
        String::from_utf8_lossy(&expected.cause),
        "oc-rsync refusal of {filter:?} {extra:?} differs from rsync 3.5.2"
    );
    assert_eq!(actual, expected, "byte-level or exit-code difference");
}

/// rsync 3.5.2 prints `invalid modifier '\#013' at position 1 in filter rule:
/// +\#013foo` and exits 1.
#[test]
fn a_vt_in_a_refused_filter_rule_is_escaped_like_upstream() {
    assert_refusal_matches_upstream(b"+\x0bfoo", &[]);
}

/// `-8` closes `ic_chck` (rsync.c:96) but log.c:499 still passes the line to
/// `filtered_fwrite()`, which escapes C0 bytes whatever `use_isprint` says.
#[test]
fn a_vt_in_a_refused_filter_rule_is_escaped_under_eight_bit_output() {
    assert_refusal_matches_upstream(b"+\x0bfoo", &["-8"]);
}

/// `filter_rule_err()` (exclude.c:133-136) echoes the whole rule, so a control
/// byte in the middle of an unknown rule is escaped in place.
#[test]
fn a_control_byte_in_an_unknown_filter_rule_is_escaped_like_upstream() {
    assert_refusal_matches_upstream(b"foo\x01bar", &[]);
}

/// A missing source operand is named on the client's own stderr, so a control
/// byte in it must be escaped there. rsync 3.5.2 prints
/// `rsync: [sender] link_stat ".../no\#001pe" failed: ...` and exits 23. The
/// line around the name is not yet worded like upstream's, so this cell checks
/// the escaping and the exit code, not the whole line.
#[test]
fn a_control_byte_in_a_missing_operand_is_escaped_on_stderr() {
    let tmp = test_support::create_tempdir();
    fs::create_dir_all(tmp.path().join("dst")).expect("mkdir dst");
    let output = Command::new(oc_rsync_bin())
        .current_dir(tmp.path())
        .env("LC_ALL", "C")
        .arg("-r")
        .arg(OsStr::from_bytes(b"no\x01pe"))
        .arg("dst/")
        .output()
        .expect("spawn oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(23), "{stderr}");
    assert!(
        !output.stderr.contains(&0x01),
        "raw control byte: {stderr:?}"
    );
    assert!(stderr.contains("no\\#001pe"), "{stderr}");
}
