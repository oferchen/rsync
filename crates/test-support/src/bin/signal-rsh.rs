//! Remote-shell trampoline that signals the server at a fixed wire offset.
//!
//! Same argv model as `capture-rsh`: leading `-*` options and the host token
//! are dropped and the rest is spawned as the server command, with both
//! directions relayed through this process. Once the direction named by
//! `OC_SIGNAL_RSH_AT` (`c2s:<bytes>` or `s2c:<bytes>`) has forwarded that many
//! bytes, the server receives `OC_SIGNAL_RSH_SIGNAL` (`INT`, `TERM` or `HUP`)
//! exactly once.
//!
//! The pump that crossed the offset sends the signal before it forwards
//! another chunk, so the server is signalled mid-transfer at a known point
//! with no timing assumption: the remaining bytes keep flowing afterwards,
//! which is what lets the server observe the signal at its next frame.

use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Command, Stdio, exit};
use std::thread;

/// Where to signal: the relay direction and the byte count after which to fire.
const AT_ENV: &str = "OC_SIGNAL_RSH_AT";
/// Which signal to send, as `kill -<name>` spells it.
const SIGNAL_ENV: &str = "OC_SIGNAL_RSH_SIGNAL";

fn required(var: &str) -> String {
    env::var(var).unwrap_or_else(|_| {
        eprintln!("signal-rsh: {var} is not set");
        exit(1);
    })
}

/// Copies `from` into `to`; once `fire_after` bytes have been forwarded, runs
/// `fire` once before forwarding anything more.
fn pump(mut from: impl Read, mut to: impl Write, mut fire_after: Option<u64>, fire: impl Fn()) {
    let mut buf = [0u8; 65536];
    let mut forwarded: u64 = 0;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if to.write_all(&buf[..n]).is_err() {
            break;
        }
        let _ = to.flush();
        forwarded += n as u64;
        if fire_after.is_some_and(|at| forwarded >= at) {
            fire_after = None;
            fire();
        }
    }
}

fn main() {
    if let Err(e) = fast_io::force_blocking_stdio() {
        eprintln!("signal-rsh: cannot clear O_NONBLOCK on stdio: {e}");
        exit(1);
    }
    let at = required(AT_ENV);
    let signal = required(SIGNAL_ENV);
    let (direction, offset) = at.split_once(':').unwrap_or_else(|| {
        eprintln!("signal-rsh: {AT_ENV} must be c2s:<bytes> or s2c:<bytes>, got {at}");
        exit(1);
    });
    let offset: u64 = offset.parse().unwrap_or_else(|_| {
        eprintln!("signal-rsh: bad byte offset in {AT_ENV}={at}");
        exit(1);
    });
    let (c2s_at, s2c_at) = match direction {
        "c2s" => (Some(offset), None),
        "s2c" => (None, Some(offset)),
        _ => {
            eprintln!("signal-rsh: {AT_ENV} direction must be c2s or s2c, got {direction}");
            exit(1);
        }
    };

    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    let first = argv
        .iter()
        .position(|arg| !arg.to_string_lossy().starts_with('-'))
        .map_or(argv.len(), |host| host + 1);
    if first >= argv.len() {
        eprintln!("signal-rsh: no server command after the host token");
        exit(1);
    }

    let mut child = Command::new(&argv[first])
        .args(&argv[first + 1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("signal-rsh: failed to spawn server: {e}");
            exit(1);
        });
    let pid = child.id().to_string();
    let fire = move || {
        let sent = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(&pid)
            .status()
            .is_ok_and(|status| status.success());
        if !sent {
            eprintln!("signal-rsh: kill -{signal} {pid} failed");
            exit(1);
        }
    };

    let child_in = child.stdin.take().expect("stdin was piped");
    let child_out = child.stdout.take().expect("stdout was piped");
    let fire_in = fire.clone();
    let t_in = thread::spawn(move || pump(std::io::stdin().lock(), child_in, c2s_at, fire_in));
    let t_out = thread::spawn(move || pump(child_out, std::io::stdout().lock(), s2c_at, fire));

    let status = child.wait().expect("wait on server child");
    // The s2c pump ends at the server's stdout EOF. The c2s pump may stay
    // parked on our stdin, which the client holds open until we exit.
    let _ = t_out.join();
    drop(t_in);
    exit(status.code().unwrap_or(255));
}
