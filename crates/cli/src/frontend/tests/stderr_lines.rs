//! Client diagnostics that upstream prints with several `rprintf(FERROR, ...)`
//! calls reach stderr as several lines, while a newline inside one operand is
//! still escaped.

use super::common::*;
use super::*;

fn stderr_of(args: &[&[u8]]) -> (i32, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    let args: Vec<OsString> = args
        .iter()
        .map(|arg| std::ffi::OsStr::from_bytes(arg).to_os_string())
        .collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(args, &mut stdout, &mut stderr);
    (status, stderr)
}

/// upstream: main.c:1591-1592 prints `Unexpected local arg: %s` and the colon
/// hint with two `rprintf(FERROR, ...)` calls. log.c:460 keeps each trailing
/// newline raw, and log.c:288-291 escapes a newline inside the operand.
#[test]
fn a_local_source_after_a_remote_one_prints_two_cause_lines() {
    let (status, stderr) =
        stderr_of(&[RSYNC.as_bytes(), b"host:/remote/a", b"/local/b\nc", b"dst/"]);
    let text = String::from_utf8(stderr).expect("escaped stderr is ASCII");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(status, 1, "{text}");
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0], "Unexpected local arg: /local/b\\#012c");
    assert_eq!(
        lines[1],
        "If arg is a remote file/dir, prefix it with a colon (:)."
    );
    assert!(
        lines[2].contains("syntax or usage error (code 1)"),
        "{text}"
    );
}

/// A command-line parse error spans several lines; each stays its own line
/// instead of being escaped into one (log.c:460).
#[test]
fn a_parse_error_keeps_its_lines() {
    let (status, stderr) = stderr_of(&[RSYNC.as_bytes(), b"--version=extra"]);
    let text = String::from_utf8(stderr).expect("escaped stderr is ASCII");
    assert_eq!(status, 1, "{text}");
    assert!(!text.contains("\\#012"), "{text}");
    assert!(text.lines().count() > 1, "{text}");
}

/// upstream: options.c:933 option_error() prints the rejected token through
/// rprintf(FERROR, ...), so a control byte in it prints as `\#001`. Measured
/// against rsync 3.5.2: `rsync: --bo\#001gus: unknown option`.
#[test]
fn an_unknown_option_is_escaped() {
    let (status, stderr) = stderr_of(&[RSYNC.as_bytes(), b"--bo\x01gus"]);
    let text = String::from_utf8_lossy(&stderr);
    assert_eq!(status, 1, "{text}");
    assert!(!stderr.contains(&0x01), "raw control byte: {text:?}");
    assert!(text.contains("--bo\\#001gus: unknown option"), "{text}");
}

/// A daemon that sends free text and closes before acknowledging the listing:
/// each line it sent is its own stderr line (clientserver.c:428-429,
/// 438-439), a control byte in one is escaped, then the refusal follows.
#[test]
fn a_daemon_refusal_prints_one_line_per_daemon_line() {
    let (addr, handle) = spawn_stub_daemon(vec!["no listing here\n", "second\x01line\n"]);
    let url = format!("rsync://{}:{}/", addr.ip(), addr.port());
    let (_code, _stdout, stderr) = run_with_args([OsString::from(RSYNC), OsString::from(url)]);
    let text = String::from_utf8(stderr).expect("escaped stderr is ASCII");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0], "no listing here");
    assert_eq!(lines[1], "second\\#001line");
    assert!(lines[2].contains("daemon refused module listing"), "{text}");
    handle.join().expect("server thread");
}
