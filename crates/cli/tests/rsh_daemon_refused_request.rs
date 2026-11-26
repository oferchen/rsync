//! A daemon started through a remote shell (`--server --daemon .`) that
//! refuses its client before any transfer exits like rsync 3.5.2.
//!
//! upstream: every refusal in `exchange_protocols()` (clientserver.c:213-244)
//! and in `start_daemon()` - an invalid `#early_input=` length
//! (clientserver.c:1551-1554), an unknown `#` command (:1568-1572) and an
//! unknown module (:1574-1579) - writes its `@ERROR` line and returns -1, which
//! `main()` returns (main.c:1978): the process exits 255 with nothing on
//! stderr. `start_daemon()` has no case for `@RSYNCD: EXIT`, so as a request
//! line it is an unknown module. EOF inside the `#early_input=` payload is
//! different: `read_buf()` (clientserver.c:1556) reaches `whine_about_eof()`
//! (io.c:299-303), which prints one line and exits `RERR_STREAMIO` (12).
//!
//! Every cell asserts the 3.5.2 result measured on glibc. When the 3.5.2
//! oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 on the same input
//! and compares exit code, stdout and stderr.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// What fd 0 is: a socket, as some remote shells hand over, or a pipe, as
/// sshd does.
#[derive(Clone, Copy, Debug)]
enum Stdin {
    Socketpair,
    Pipe,
}

/// A refusal `start_daemon()` answers and returns -1 from: what the client
/// sends, and the reply that follows the daemon's greeting.
const REFUSED: [(&str, &[u8], &str); 6] = [
    (
        "malformed greeting",
        b"hello\n",
        "@ERROR: protocol startup error\n",
    ),
    (
        "greeting without a digest list",
        b"@RSYNCD: 32.0\n",
        "@ERROR: your client omitted the digest name list: @RSYNCD: 32.0\n",
    ),
    (
        "unknown # command",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n#foo\n",
        "@ERROR: Unknown command '#foo'\n",
    ),
    (
        "unknown module",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nnosuch\n",
        "@ERROR: Unknown module 'nosuch'\n",
    ),
    (
        "@RSYNCD: EXIT as the request line",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n@RSYNCD: EXIT\n",
        "@ERROR: Unknown module '@RSYNCD: EXIT'\n",
    ),
    (
        "invalid early_input length",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n#early_input=0\n",
        "@ERROR: invalid early_input length\n",
    ),
];

/// The client closes inside the `#early_input=` payload: before its first
/// byte, and part-way through.
const EARLY_INPUT_EOF: [(&str, &[u8]); 2] = [
    (
        "EOF before the early_input payload",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n#early_input=10\n",
    ),
    (
        "EOF inside the early_input payload",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n#early_input=10\nabc",
    ),
];

/// upstream: io.c:299-301 - `stats.total_read` is still 0 because the lines
/// before the payload were read by `read_line_old()`.
const EOF_LINE: &str = "rsync: connection unexpectedly closed (0 bytes received so far) [Receiver]";

/// Runs `binary` as an rsh-run daemon over a one-module config, feeds it
/// `input` on `stdin`, then closes the client side.
fn run_daemon(binary: &Path, stdin: Stdin, input: &[u8]) -> Output {
    let dir = TempDir::new().expect("tempdir");
    fs::create_dir(dir.path().join("mod")).expect("mkdir mod");
    let conf = dir.path().join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = no\n[mod]\npath = {}\n",
            dir.path().join("mod").display()
        ),
    )
    .expect("write config");
    let mut command = Command::new(binary);
    command
        .arg(format!("--config={}", conf.display()))
        .args(["--server", "--daemon", "."])
        .current_dir(dir.path())
        .env_remove("REMOTE_HOST")
        .env_remove("SSH_CLIENT")
        .env_remove("SSH2_CLIENT")
        .env("SSH_CONNECTION", "127.0.0.1 40000 127.0.0.1 22")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Stdin::Socketpair => {
            let (ours, mut theirs) = UnixStream::pair().expect("socketpair");
            theirs.write_all(input).expect("send client bytes");
            // Half-close: the daemon reads EOF after the input, while its
            // replies still have a reader until it exits.
            theirs
                .shutdown(std::net::Shutdown::Write)
                .expect("half-close the client end");
            let output = command
                .stdin(Stdio::from(std::os::fd::OwnedFd::from(ours)))
                .output()
                .expect("run daemon");
            drop(theirs);
            output
        }
        Stdin::Pipe => {
            let mut child = command.stdin(Stdio::piped()).spawn().expect("spawn daemon");
            let mut pipe = child.stdin.take().expect("daemon stdin");
            pipe.write_all(input).expect("send client bytes");
            drop(pipe);
            child.wait_with_output().expect("wait for daemon")
        }
    }
}

/// The daemon's reply after its own greeting line.
fn reply(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (greeting, rest) = stdout.split_once('\n').expect("a greeting line");
    assert!(greeting.starts_with("@RSYNCD: "), "{output:?}");
    rest.to_owned()
}

fn check_refused(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (name, input, expected_reply) in REFUSED {
        let oc = run_daemon(&oc_rsync_bin(), stdin, input);
        let cell = format!("{stdin:?} / {name}");
        assert_eq!(oc.status.code(), Some(255), "{cell}: {oc:?}");
        assert_eq!(reply(&oc), expected_reply, "{cell}: {oc:?}");
        assert!(oc.stderr.is_empty(), "{cell}: {oc:?}");
        if let Some(upstream) = upstream.as_ref() {
            let reference = run_daemon(upstream.binary(), stdin, input);
            assert_eq!(
                (oc.status.code(), &oc.stdout, &oc.stderr),
                (
                    reference.status.code(),
                    &reference.stdout,
                    &reference.stderr
                ),
                "{cell}: oc differs from rsync 3.5.2"
            );
        }
    }
}

/// Splits stderr into the EOF line and the exit trailer after it.
fn eof_report(output: &Output) -> (String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (line, trailer) = stderr.split_once('\n').expect("two stderr lines");
    (line.to_owned(), trailer.to_owned())
}

/// The trailer names each implementation's own program, source location and
/// version, so only its fixed parts are compared: the description, the code
/// and the role.
fn assert_stream_io_trailer(trailer: &str, cell: &str) {
    assert!(
        trailer.contains(" error: error in rsync protocol data stream (code 12) at ")
            && trailer.contains(" [Receiver=")
            && trailer.ends_with("]\n")
            && trailer.matches('\n').count() == 1,
        "{cell}: {trailer:?}"
    );
}

fn check_early_input_eof(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (name, input) in EARLY_INPUT_EOF {
        let oc = run_daemon(&oc_rsync_bin(), stdin, input);
        let cell = format!("{stdin:?} / {name}");
        assert_eq!(oc.status.code(), Some(12), "{cell}: {oc:?}");
        assert_eq!(reply(&oc), "", "{cell}: {oc:?}");
        let (line, trailer) = eof_report(&oc);
        assert_eq!(line, EOF_LINE, "{cell}: {oc:?}");
        assert_stream_io_trailer(&trailer, &cell);
        if let Some(upstream) = upstream.as_ref() {
            let reference = run_daemon(upstream.binary(), stdin, input);
            let (reference_line, reference_trailer) = eof_report(&reference);
            assert_eq!(
                (oc.status.code(), &oc.stdout, &line),
                (reference.status.code(), &reference.stdout, &reference_line),
                "{cell}: oc differs from rsync 3.5.2"
            );
            assert_stream_io_trailer(&reference_trailer, &cell);
        }
    }
}

#[test]
fn socket_stdin_refused_request_exits_255() {
    check_refused(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_refused_request_exits_255() {
    check_refused(Stdin::Pipe);
}

#[test]
fn socket_stdin_closed_inside_early_input_exits_12() {
    check_early_input_eof(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_closed_inside_early_input_exits_12() {
    check_early_input_eof(Stdin::Pipe);
}
