//! Teeing remote-shell trampoline for wire-transcript capture.
//!
//! Behaves like the repo's `fake_rsh.sh` shims: leading `-*` options and the
//! host token are dropped, and the remaining argv is spawned as the server
//! command. Unlike those shims it interposes on the client<->server pipe pair
//! and appends every byte crossing each direction to a capture file:
//!
//! - client -> server bytes go to the file named by `OC_TRANSCRIPT_C2S`;
//! - server -> client bytes go to the file named by `OC_TRANSCRIPT_S2C`.
//!
//! Nothing is interpreted or reframed. Upstream keeps the multiplex framing
//! inside this same byte stream (the 4-byte `(MPLEX_BASE + code) << 24 | len`
//! header, upstream: io.c:1155 io_flush/mplex writer), so capturing the raw
//! stream captures negotiation, multiplex frames and payload alike.
//!
//! Each pump forwards a chunk as soon as one `read` returns rather than
//! waiting to fill a buffer: the rsync handshake is a strict request/reply
//! ping-pong, and a relay that buffers past a message boundary deadlocks both
//! peers (the failure mode of a naive buffered relay).

use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio, exit};
use std::thread;
use std::time::Duration;

use test_support::transcript::{TRANSCRIPT_C2S_ENV as C2S_ENV, TRANSCRIPT_S2C_ENV as S2C_ENV};

fn capture_path(var: &str) -> PathBuf {
    match env::var_os(var) {
        Some(path) => PathBuf::from(path),
        None => {
            eprintln!("capture-rsh: {var} is not set; refusing to run without a capture sink");
            exit(1);
        }
    }
}

/// Copy `from` into `to`, appending every forwarded byte to `log`.
///
/// Ends on EOF or when the destination refuses more bytes (the peer closed
/// its end - normal shutdown for a bidirectional pipe pair). A capture-file
/// write failure aborts the process instead: a silently short transcript
/// would let a byte-identity comparison pass on truncated evidence.
fn pump(mut from: impl Read, mut to: impl Write, mut log: File, direction: &str) {
    let mut buf = [0u8; 65536];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Err(e) = log.write_all(&buf[..n]) {
            eprintln!("capture-rsh: {direction} capture write failed: {e}");
            exit(1);
        }
        if to.write_all(&buf[..n]).is_err() {
            break;
        }
        // Stdout is line-buffered; the protocol is binary and latency-bound.
        let _ = to.flush();
    }
}

fn main() {
    let c2s = capture_path(C2S_ENV);
    let s2c = capture_path(S2C_ENV);

    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    // Same argv model as fake_rsh.sh: skip leading `-*` flags, skip the host
    // token, exec the rest as the server command.
    let mut i = 0;
    while i < argv.len() && argv[i].to_string_lossy().starts_with('-') {
        i += 1;
    }
    // The host token itself.
    i += 1;
    if i >= argv.len() {
        eprintln!("capture-rsh: no server command after the host token");
        exit(1);
    }

    let c2s_log =
        File::create(&c2s).unwrap_or_else(|e| panic!("capture-rsh: create {}: {e}", c2s.display()));
    let s2c_log =
        File::create(&s2c).unwrap_or_else(|e| panic!("capture-rsh: create {}: {e}", s2c.display()));

    let mut child = match Command::new(&argv[i])
        .args(&argv[i + 1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!(
                "capture-rsh: failed to spawn {}: {e}",
                argv[i].to_string_lossy()
            );
            exit(1);
        }
    };

    let child_in = child.stdin.take().expect("stdin was piped");
    let child_out = child.stdout.take().expect("stdout was piped");

    // Dropping `child_in` at the pump's end closes the server's stdin, so an
    // EOF from the client propagates as a half-close exactly like a real rsh.
    let t_in = thread::spawn(move || pump(std::io::stdin().lock(), child_in, c2s_log, "c2s"));
    let t_out = thread::spawn(move || pump(child_out, std::io::stdout().lock(), s2c_log, "s2c"));

    let status = child.wait().expect("wait on server child");
    // The s2c pump ends deterministically at the child's stdout EOF.
    let _ = t_out.join();
    // The c2s pump may still be blocked reading OUR stdin if the client keeps
    // its write end open after the server exits; every byte the server
    // consumed has already been logged by then, so wait briefly rather than
    // forever (the client is usually waiting on our exit).
    for _ in 0..200 {
        if t_in.is_finished() {
            let _ = t_in.join();
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    exit(status.code().unwrap_or(1));
}
