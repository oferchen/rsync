//! Client terminal output escapes file names as rsync 3.5.2 does in the
//! process locale.
//!
//! upstream: main.c:1911 adopts the environment's `LC_CTYPE`; rsync.c:95-98
//! opens `ic_chck` for a client running without `-8`; log.c:462-494 `rwrite()`
//! then writes each byte that does not start a valid character as `\#ooo` and
//! passes the valid characters to `filtered_fwrite(.., 0, ..)` (log.c:476),
//! which keeps a printable multibyte character raw and escapes C0 (tab
//! excepted), DEL and the C1 controls (log.c:276-293). So under a UTF-8 locale
//! `café` prints raw, and under `C` it prints as `caf\#303\#251`.
//!
//! # Gating
//!
//! Each cell runs oc-rsync and upstream 3.5.2 on the same tree and compares
//! stdout byte for byte. It self-skips unless `OC_RSYNC_UPSTREAM_COMPAT=1` and
//! an upstream 3.5.2 binary resolves (override with
//! `OC_RSYNC_UPSTREAM_BIN_3_5_2`); with the variable set, a missing oracle
//! fails the cell. Names that are not valid UTF-8 are Linux-only: APFS refuses
//! to create them.
#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::{self, File};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// A UTF-8 locale both binaries can adopt on the host the cell runs on.
const UTF8_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// Names every Unix filesystem can hold, one per escaping class.
const PORTABLE_NAMES: &[&[u8]] = &[
    // Printable two-byte character: raw under UTF-8, escaped under C.
    b"caf\xc3\xa9",
    // NO-BREAK SPACE U+00A0: printable, so raw under UTF-8.
    b"nb\xc2\xa0x",
    // C1 control U+0085: escaped byte by byte in every locale.
    b"c1\xc2\x85x",
    // C0 controls and DEL: always escaped; tab is never escaped.
    b"esc\x1bx",
    b"cr\rx",
    b"del\x7f",
    b"tab\tx",
    // A literal escape sequence gets its backslash escaped.
    b"lit\\#123",
];

/// Names only Linux can hold: each carries bytes that are not valid UTF-8.
#[cfg(target_os = "linux")]
const INVALID_UTF8_NAMES: &[&[u8]] = &[
    // A lone byte that never starts a character.
    b"bad\xff",
    // A lead byte cut off at the end of the name.
    b"trunc\xc3",
    // An encoded UTF-16 surrogate, which UTF-8 forbids.
    b"sur\xed\xa0\x80",
    // A lone C1 byte.
    b"lone\x85x",
];

#[cfg(not(target_os = "linux"))]
const INVALID_UTF8_NAMES: &[&[u8]] = &[];

/// Builds `root/src` holding one file per name, every timestamp pinned so the
/// two listings carry identical dates.
fn build(root: &Path) {
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    let src = root.join("src");
    fs::create_dir_all(&src).expect("mkdir src");
    for name in PORTABLE_NAMES.iter().chain(INVALID_UTF8_NAMES) {
        let path = src.join(OsStr::from_bytes(name));
        fs::write(&path, b"x").expect("write fixture");
        File::open(&path)
            .and_then(|file| file.set_modified(stamp))
            .expect("set file mtime");
    }
    File::open(&src)
        .and_then(|dir| dir.set_modified(stamp))
        .expect("set dir mtime");
}

/// Runs `command` under `locale` and returns stdout without the transfer
/// summary, whose byte counts and rate are outside this cell's subject.
fn stdout_under(mut command: Command, locale: &str) -> Vec<u8> {
    let output = command.env("LC_ALL", locale).output().expect("spawn rsync");
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
        .stdout
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| !line.starts_with(b"sent ") && !line.starts_with(b"total size "))
        .flatten()
        .copied()
        .collect()
}

/// Compares oc-rsync and upstream 3.5.2 stdout for `args`. Each run starts in
/// its own working directory and copies into the relative destination `dst`
/// there, so neither sees the other's files and the `created directory` line
/// names the same path.
fn assert_matches_upstream(locale: &str, args: &[&str], copies: bool) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let tmp = test_support::create_tempdir();
    build(tmp.path());
    let src = format!("{}/", tmp.path().join("src").display());
    let run = |mut command: Command, cwd: &str| {
        let cwd = tmp.path().join(cwd);
        fs::create_dir_all(&cwd).expect("mkdir cwd");
        command.current_dir(&cwd).args(args).arg(&src);
        if copies {
            command.arg("dst");
        }
        stdout_under(command, locale)
    };
    let expected = run(upstream.command(), "upstream");
    let actual = run(Command::new(oc_rsync_bin()), "oc");
    assert_eq!(
        String::from_utf8_lossy(&actual),
        String::from_utf8_lossy(&expected),
        "oc-rsync {args:?} under LC_ALL={locale} differs from rsync 3.5.2"
    );
    assert_eq!(actual, expected, "byte-level difference under {locale}");
}

#[test]
fn list_only_under_utf8_locale_matches_upstream() {
    assert_matches_upstream(UTF8_LOCALE, &["-r", "--list-only"], false);
}

#[test]
fn list_only_under_c_locale_matches_upstream() {
    assert_matches_upstream("C", &["-r", "--list-only"], false);
}

#[test]
fn verbose_copy_under_utf8_locale_matches_upstream() {
    assert_matches_upstream(UTF8_LOCALE, &["-rv"], true);
}

#[test]
fn verbose_copy_under_c_locale_matches_upstream() {
    assert_matches_upstream("C", &["-rv"], true);
}

#[test]
fn itemized_copy_under_utf8_locale_matches_upstream() {
    assert_matches_upstream(UTF8_LOCALE, &["-ri"], true);
}

#[test]
fn itemized_copy_under_c_locale_matches_upstream() {
    assert_matches_upstream("C", &["-ri"], true);
}

/// `-8` closes `ic_chck` (rsync.c:95), so a UTF-8 locale passes every byte
/// that is not a control, valid or not.
#[test]
fn eight_bit_verbose_copy_under_utf8_locale_matches_upstream() {
    assert_matches_upstream(UTF8_LOCALE, &["-rv8"], true);
}
