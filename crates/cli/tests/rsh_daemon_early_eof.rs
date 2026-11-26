//! A daemon started through a remote shell (`--server --daemon .`) whose
//! client goes away before sending a request line exits like rsync 3.5.2.
//!
//! upstream: `read_line_old()` returns 0 at EOF (io.c:2642-2651), so
//! `exchange_protocols()` (clientserver.c:206-210) or the request read in
//! `start_daemon()` (clientserver.c:1546-1547) returns -1, and `main()`
//! returns that from `start_daemon()` (main.c:1978): the process exits 255
//! with nothing on stderr. The daemon still writes its greeting first.
//!
//! Every cell asserts the 3.5.2 result measured on glibc: exit 255, the
//! greeting on stdout, empty stderr. When the 3.5.2 oracle resolves
//! (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
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

const CLIENT_GREETING: &[u8] = b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

/// What fd 0 is: a socket, as some remote shells hand over, or a pipe, as
/// sshd does.
#[derive(Clone, Copy, Debug)]
enum Stdin {
    Socketpair,
    Pipe,
}

/// What the client sends before it closes its end.
const CELLS: [(&str, &[u8]); 3] = [
    ("nothing", b""),
    ("greeting only", CLIENT_GREETING),
    (
        "greeting and part of a module name",
        b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nmo",
    ),
];

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
            // Half-close: the daemon reads EOF, while its greeting write
            // still has a reader until it exits.
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

fn check(stdin: Stdin) {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (name, input) in CELLS {
        let oc = run_daemon(&oc_rsync_bin(), stdin, input);
        let cell = format!("{stdin:?} / {name}");
        assert_eq!(oc.status.code(), Some(255), "{cell}: {oc:?}");
        assert!(oc.stdout.starts_with(b"@RSYNCD: "), "{cell}: {oc:?}");
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

#[test]
fn socket_stdin_closed_before_a_request_line_exits_255() {
    check(Stdin::Socketpair);
}

#[test]
fn pipe_stdin_closed_before_a_request_line_exits_255() {
    check(Stdin::Pipe);
}
