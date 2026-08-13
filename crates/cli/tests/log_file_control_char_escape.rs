//! `--log-file` escapes control characters in filenames (CWE-117).
//!
//! Upstream writes every log-file line through `logit()`, which since rsync
//! 3.5.0 filters the message before it reaches `logfile_fp`:
//!
//! ```c
//! int len = strlen(buf);
//! char trailing = len && (buf[len-1] == '\n' || buf[len-1] == '\r') ? buf[--len] : '\0';
//! fprintf(logfile_fp, "%s [%d] ", timestring(time(NULL)), (int)getpid());
//! filtered_fwrite(logfile_fp, buf, len, 0, 1, trailing);
//! ```
//!
//! (`log.c:126-131`). The `0, 1` pair is `use_isprint = 0, escape_c1 = 1`, so
//! `filtered_fwrite` (`log.c:242-263`) escapes, as `\#ooo`:
//!
//! - every byte below 0x20 except tab, which covers `LF` and `ESC`; and
//! - the C1 range 0x80-0x9F, which covers the 8-bit `CSI` (0x9B).
//!
//! and leaves everything else - notably `DEL` (0x7F) and 0xA0-0xFF - verbatim,
//! so a non-UTF-8 name stays readable in the log. The pair is fixed: unlike the
//! terminal sink (`log.c:425`, `!allow_8bit_chars`), it is not reachable from
//! `--8-bit-output`, so `-8` must not weaken the log file.
//!
//! Expectations below were captured byte-for-byte from rsync 3.5.0 running the
//! same layout with `--log-file` at both default verbosity and under `-8`.
//!
//! The fixtures are split by what the host filesystem can name. Every Unix
//! runs [`PORTABLE_NAME`]; only Linux runs [`NON_UTF8_NAME`] - see the
//! `non_utf8_names` module for why.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Source filename carrying the interesting bytes that every Unix filesystem
/// can name: `ESC` (0x1B) and `LF` (0x0A), which must be escaped, plus `DEL`
/// (0x7F) and a tab, which must survive verbatim.
const PORTABLE_NAME: &[u8] = b"A\x1bB\nC\x7fD\tE";

/// [`PORTABLE_NAME`] as the log file must render it.
const PORTABLE_RENDERING: &[u8] = b"A\\#033B\\#012C\x7fD\tE";

/// Builds `src/<name>` and returns the temp root.
fn named_tree(name: &[u8]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join(OsStr::from_bytes(name)), b"payload\n").expect("write source");
    tmp
}

/// Runs a transfer that logs the transferred name and returns the log bytes.
fn log_bytes(root: &Path, dest: &str, extra: &[&str]) -> Vec<u8> {
    let log = root.join(format!("{dest}.log"));
    let mut command = Command::new(oc_rsync_bin());
    command
        .arg("-a")
        .args(extra)
        .arg(format!("--log-file={}", log.display()))
        .arg("--log-file-format=%i %n")
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("{}/", root.join(dest).display()));
    let status = command.status().expect("run oc-rsync");
    assert!(status.success(), "transfer failed: {status:?}");
    fs::read(&log).expect("read log file")
}

/// The escaped leader shared by both fixtures; identifies the entry's log
/// record without depending on which itemize letters precede it.
const ESCAPED_LEADER: &[u8] = b"A\\#033B";

/// Returns every log line that carries the transferred filename.
fn name_lines(log: &[u8]) -> Vec<Vec<u8>> {
    log.split(|&byte| byte == b'\n')
        .filter(|line| {
            line.windows(ESCAPED_LEADER.len())
                .any(|window| window == ESCAPED_LEADER)
        })
        .map(<[u8]>::to_vec)
        .collect()
}

/// Returns the single log line that carries the transferred filename.
fn name_line(log: &[u8]) -> Vec<u8> {
    let mut lines = name_lines(log);
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one record for the entry, got {}: {}",
        lines.len(),
        String::from_utf8_lossy(log)
    );
    lines.remove(0)
}

/// Asserts the log renders `name` exactly as rsync 3.5.0 does.
fn assert_rendering(name: &[u8], expected: &[u8]) {
    let tmp = named_tree(name);
    let line = name_line(&log_bytes(tmp.path(), "dst", &[]));
    assert!(
        line.ends_with(expected),
        "log line {:?} does not match the rsync 3.5.0 rendering",
        String::from_utf8_lossy(&line)
    );
}

/// Asserts `-8` leaves the log-file rendering of `name` untouched.
fn assert_eight_bit_does_not_weaken(name: &[u8]) {
    let tmp = named_tree(name);
    let plain = name_line(&log_bytes(tmp.path(), "dst", &[]));
    let eight_bit = name_line(&log_bytes(tmp.path(), "dst8", &["-8"]));
    let strip_prefix = |line: Vec<u8>| {
        let start = line
            .windows(ESCAPED_LEADER.len())
            .position(|window| window == ESCAPED_LEADER)
            .expect("escaped name leader");
        line[start..].to_vec()
    };
    assert_eq!(strip_prefix(plain), strip_prefix(eight_bit));
}

/// Asserts no raw `ESC` or C1 byte reaches any part of the log for `name`.
fn assert_no_raw_escape_or_c1(name: &[u8]) {
    let tmp = named_tree(name);
    for extra in [&[][..], &["-8"][..]] {
        let dest = if extra.is_empty() { "dst" } else { "dst8" };
        let log = log_bytes(tmp.path(), dest, extra);
        assert!(
            !log.iter()
                .any(|&byte| byte == 0x1B || (0x80..=0x9F).contains(&byte)),
            "raw ESC or C1 byte reached the log: {:?}",
            String::from_utf8_lossy(&log)
        );
        // Exactly one line for the entry: an embedded LF must have been escaped
        // rather than splitting the record in two.
        assert_eq!(
            name_lines(&log).len(),
            1,
            "the embedded newline split the log record"
        );
    }
}

/// upstream: log.c:126-131 - `LF` and `ESC` are escaped as `\#ooo`, so a
/// filename cannot forge a log line or drive the terminal of an operator who
/// later `cat`s the log; `DEL` and tab survive verbatim because the log sink
/// runs with `use_isprint = 0`.
#[test]
fn log_file_escapes_controls_but_keeps_del() {
    assert_rendering(PORTABLE_NAME, PORTABLE_RENDERING);
}

/// upstream: log.c:131 passes a fixed `use_isprint = 0, escape_c1 = 1` pair, so
/// the log-file rendering is identical with and without `--8-bit-output`. A
/// name-supplied control byte must not become raw just because `-8` was given.
#[test]
fn log_file_escaping_is_not_weakened_by_eight_bit_output() {
    assert_eight_bit_does_not_weaken(PORTABLE_NAME);
}

/// No raw `ESC` byte may reach the log file anywhere - not just on the itemize
/// line - or an attacker-chosen name could rewrite what the operator sees for
/// surrounding entries.
#[test]
fn log_file_contains_no_raw_escape_bytes() {
    assert_no_raw_escape_or_c1(PORTABLE_NAME);
}

/// The same contract exercised with a filename that is not valid UTF-8.
///
/// This is Linux-only because macOS/APFS rejects a filename containing a byte
/// sequence that is not valid UTF-8, failing `write(2)` with `EILSEQ`
/// ("Illegal byte sequence") - so [`NON_UTF8_NAME`] cannot be created there at
/// all. That is a platform limit on constructing the *fixture*, not a relaxed
/// expectation: the escaping contract is identical on both platforms, and the
/// bytes macOS *can* name are covered by the tests above, which run everywhere.
/// Nothing here is weakened or skipped for convenience.
#[cfg(target_os = "linux")]
mod non_utf8_names {
    use super::{assert_eight_bit_does_not_weaken, assert_no_raw_escape_or_c1, assert_rendering};

    /// Adds the two bytes macOS cannot name: the 8-bit `CSI` (0x9B), which must
    /// be escaped because it is in the C1 range, and 0xFF, which must survive
    /// verbatim because it is above it.
    const NON_UTF8_NAME: &[u8] = b"A\x1bB\nC\x9bD\x7fE\xffF\tG";

    /// [`NON_UTF8_NAME`] as the log file must render it.
    const NON_UTF8_RENDERING: &[u8] = b"A\\#033B\\#012C\\#233D\x7fE\xffF\tG";

    /// upstream: log.c:126-131 - the 8-bit `CSI` is escaped as `\#233` because
    /// `escape_c1 = 1`, while 0xFF survives verbatim because `use_isprint = 0`,
    /// keeping a non-UTF-8 name readable in the log.
    #[test]
    fn log_file_escapes_c1_but_keeps_high_bytes() {
        assert_rendering(NON_UTF8_NAME, NON_UTF8_RENDERING);
    }

    /// `-8` must not turn the C1 byte raw.
    #[test]
    fn log_file_c1_escaping_is_not_weakened_by_eight_bit_output() {
        assert_eight_bit_does_not_weaken(NON_UTF8_NAME);
    }

    /// No raw C1 byte may reach the log file anywhere.
    #[test]
    fn log_file_contains_no_raw_c1_bytes() {
        assert_no_raw_escape_or_c1(NON_UTF8_NAME);
    }
}
