//! Remote-shell trampoline that rewrites one byte pattern in one direction.
//!
//! Same argv model as `capture-rsh`: leading `-*` flags and the host token are
//! dropped and the rest runs as the server command, with this process relaying
//! both directions. In the direction named by `OC_REWRITE_DIR` (`c2s` or
//! `s2c`) the first occurrence of `OC_REWRITE_FROM` is replaced by
//! `OC_REWRITE_TO`, which a test uses to make a well-formed peer send a
//! crafted file list (for example `d/Mxyz` arriving as `dM/xyz`).
//!
//! Both patterns must be the same length, so multiplex frame headers stay
//! valid. A pattern that never appeared is an error (exit 1 after the server
//! exits): a test must not pass on a relay that rewrote nothing.

#[cfg(unix)]
fn main() {
    use std::env;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio, exit};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    fn var(name: &str) -> Vec<u8> {
        match env::var_os(name) {
            Some(value) => value.into_encoded_bytes(),
            None => {
                eprintln!("rewrite-rsh: {name} is not set");
                exit(1);
            }
        }
    }

    /// Length of the longest suffix of `data` that is a proper prefix of `pattern`.
    fn partial_tail(data: &[u8], pattern: &[u8]) -> usize {
        (1..pattern.len().min(data.len() + 1))
            .rev()
            .find(|&n| data.ends_with(&pattern[..n]))
            .unwrap_or(0)
    }

    /// Copies `from` into `to`, replacing the first `pattern` with
    /// `replacement` (unchanged when `pattern` is empty). `rewrote` is set
    /// before the rewritten bytes are written, so it is set by the time the
    /// receiving side has read them.
    ///
    /// Bytes that could start the pattern are held back until more input
    /// arrives or the stream ends; there is no timer. That cannot stall the
    /// transfer only while the peer never waits on the other side holding a
    /// partial match. The tests guarantee it: under `--no-inc-recursive` the
    /// sender writes the whole file list before it waits, and every name is
    /// followed by its mode, mtime and the rest of the list; the bytes before
    /// the list are fixed by `--protocol` and `--checksum-seed`, and none of
    /// them starts the pattern. Once the replacement is made nothing is held.
    fn relay(
        mut from: impl Read,
        mut to: impl Write,
        pattern: &[u8],
        replacement: &[u8],
        rewrote: &AtomicBool,
    ) {
        let mut buf = [0u8; 65536];
        let mut pending: Vec<u8> = Vec::new();
        let mut done = pattern.is_empty();
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buf[..n]);
            let keep = if done {
                0
            } else if let Some(at) = pending.windows(pattern.len()).position(|w| w == pattern) {
                pending[at..at + pattern.len()].copy_from_slice(replacement);
                rewrote.store(true, Ordering::SeqCst);
                done = true;
                0
            } else {
                partial_tail(&pending, pattern)
            };
            let send = pending.len() - keep;
            if to
                .write_all(&pending[..send])
                .and_then(|()| to.flush())
                .is_err()
            {
                return;
            }
            pending.drain(..send);
        }
        let _ = to.write_all(&pending).and_then(|()| to.flush());
    }

    if let Err(error) = fast_io::force_blocking_stdio() {
        eprintln!("rewrite-rsh: cannot clear O_NONBLOCK on stdio: {error}");
        exit(1);
    }
    let pattern = var("OC_REWRITE_FROM");
    let replacement = var("OC_REWRITE_TO");
    if pattern.is_empty() || pattern.len() != replacement.len() {
        eprintln!(
            "rewrite-rsh: OC_REWRITE_FROM and OC_REWRITE_TO must be non-empty and the same length"
        );
        exit(1);
    }
    let client_to_server = match var("OC_REWRITE_DIR").as_slice() {
        b"c2s" => true,
        b"s2c" => false,
        _ => {
            eprintln!("rewrite-rsh: OC_REWRITE_DIR must be c2s or s2c");
            exit(1);
        }
    };
    let (c2s, s2c) = if client_to_server {
        ((pattern, replacement), (Vec::new(), Vec::new()))
    } else {
        ((Vec::new(), Vec::new()), (pattern, replacement))
    };

    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    let mut i = argv
        .iter()
        .position(|arg| !arg.to_string_lossy().starts_with('-'))
        .unwrap_or(argv.len());
    // The host token itself.
    i += 1;
    if i >= argv.len() {
        eprintln!("rewrite-rsh: no server command after the host token");
        exit(1);
    }
    let mut child = match Command::new(&argv[i])
        .args(&argv[i + 1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "rewrite-rsh: failed to spawn {}: {error}",
                argv[i].to_string_lossy()
            );
            exit(1);
        }
    };
    let child_in = child.stdin.take().expect("stdin was piped");
    let child_out = child.stdout.take().expect("stdout was piped");
    let rewrote = Arc::new(AtomicBool::new(false));
    let (up, down) = (Arc::clone(&rewrote), Arc::clone(&rewrote));
    // The client->server pump ends with the client, after the server exits,
    // so only the server->client pump is joined.
    thread::spawn(move || relay(std::io::stdin(), child_in, &c2s.0, &c2s.1, &up));
    let to_client =
        thread::spawn(move || relay(child_out, std::io::stdout().lock(), &s2c.0, &s2c.1, &down));
    let status = child.wait().expect("wait on server child");
    let _ = to_client.join();
    if !rewrote.load(Ordering::SeqCst) {
        eprintln!("rewrite-rsh: the pattern never appeared");
        exit(1);
    }
    exit(status.code().unwrap_or(1));
}

#[cfg(not(unix))]
fn main() {
    eprintln!("rewrite-rsh: only Unix remote shells pass the connection on stdio");
    std::process::exit(1);
}
