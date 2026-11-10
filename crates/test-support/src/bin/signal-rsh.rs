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
//!
//! The client learns the server's code from this process's exit status, and
//! rsync 3.5.1 reads it with a non-blocking wait once its side of the
//! connection closes (cleanup.c:146-153). A process closes its descriptors
//! before it becomes waitable, so a client that sees the close first finds
//! nothing to reap and keeps its own `RERR_STREAMIO`. On Unix a holder
//! process therefore keeps the client's descriptors open until this process
//! has been reparented away from it, then closes them.
//!
//! The holder also relays the client-to-server direction and the main thread
//! relays the other, so on Unix this process never starts a thread. A
//! process becomes waitable only once its last thread is released, while the
//! holder is reparented as soon as the main thread exits; a second thread,
//! even a joined one, can still be in its exit then, and the client could see
//! the close in between.

use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::process::ChildStdin;
use std::process::{Command, Stdio, exit};
use std::thread;

/// Where to signal: the relay direction and the byte count after which to fire.
const AT_ENV: &str = "OC_SIGNAL_RSH_AT";
/// Which signal to send, as `kill -<name>` spells it.
const SIGNAL_ENV: &str = "OC_SIGNAL_RSH_SIGNAL";
/// Set in the holder child: the socket its parent listens on.
#[cfg(unix)]
const HOLD_ENV: &str = "OC_SIGNAL_RSH_HOLD";
/// Set in the holder child: the server's pid, for a `c2s` signal.
#[cfg(unix)]
const SERVER_ENV: &str = "OC_SIGNAL_RSH_SERVER";

fn required(var: &str) -> String {
    env::var(var).unwrap_or_else(|_| {
        eprintln!("signal-rsh: {var} is not set");
        exit(1);
    })
}

/// Parses `OC_SIGNAL_RSH_AT` into the `(c2s, s2c)` byte offsets to fire at.
fn offsets() -> (Option<u64>, Option<u64>) {
    let at = required(AT_ENV);
    let (direction, offset) = at.split_once(':').unwrap_or_else(|| {
        eprintln!("signal-rsh: {AT_ENV} must be c2s:<bytes> or s2c:<bytes>, got {at}");
        exit(1);
    });
    let offset: u64 = offset.parse().unwrap_or_else(|_| {
        eprintln!("signal-rsh: bad byte offset in {AT_ENV}={at}");
        exit(1);
    });
    match direction {
        "c2s" => (Some(offset), None),
        "s2c" => (None, Some(offset)),
        _ => {
            eprintln!("signal-rsh: {AT_ENV} direction must be c2s or s2c, got {direction}");
            exit(1);
        }
    }
}

/// Returns the action that sends `OC_SIGNAL_RSH_SIGNAL` to `pid`.
fn signaller(pid: String) -> impl Fn() {
    let signal = required(SIGNAL_ENV);
    move || {
        let sent = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(&pid)
            .status()
            .is_ok_and(|status| status.success());
        if !sent {
            eprintln!("signal-rsh: kill -{signal} {pid} failed");
            exit(1);
        }
    }
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

/// Holder mode: relay the client's stdin to the server's (this process's
/// stdout) and keep the client's descriptors open until the parent relay has
/// exited and this process has been reparented, then exit. Stderr is the
/// client's stdout, held only to keep it open.
///
/// The connected socket reaches EOF when the parent closes its descriptors;
/// the reparent follows in the parent's exit, under the lock a client's wait
/// takes, so once it is seen the parent is waitable.
#[cfg(unix)]
fn hold(socket: &str) -> ! {
    let parent = std::os::unix::process::parent_id();
    let mut link = UnixStream::connect(socket).unwrap_or_else(|_| exit(1));
    let (c2s_at, _) = offsets();
    let fire = signaller(required(SERVER_ENV));
    thread::spawn(move || {
        pump(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            c2s_at,
            fire,
        )
    });
    let mut sink = [0u8; 1];
    while !matches!(link.read(&mut sink), Ok(0) | Err(_)) {}
    // EOF means the parent has run exit_files(); its exit_notify(), which
    // reparents this process and, the parent having no other thread, marks it
    // waitable under the lock a client's wait4() takes, follows in the same
    // exit path. The spin only covers that step, so it always ends and its
    // result does not depend on scheduling; a sleep would replace that
    // ordering with a guess.
    while std::os::unix::process::parent_id() == parent {
        thread::yield_now();
    }
    exit(0);
}

/// Starts the holder on the server's stdin and returns the connection whose
/// close tells it that this process is exiting. The connection must stay
/// open until exit.
#[cfg(unix)]
fn spawn_holder(server_in: ChildStdin, server_pid: u32) -> UnixStream {
    let socket = env::temp_dir().join(format!("signal-rsh-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&socket).unwrap_or_else(|e| {
        eprintln!("signal-rsh: cannot listen on {}: {e}", socket.display());
        exit(1);
    });
    let exe = env::current_exe().unwrap_or_else(|e| {
        eprintln!("signal-rsh: cannot locate itself: {e}");
        exit(1);
    });
    let client_out = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .unwrap_or_else(|e| {
            eprintln!("signal-rsh: cannot duplicate stdout: {e}");
            exit(1);
        });
    // Never waited for: the holder outlives this process by design.
    #[allow(clippy::zombie_processes)]
    Command::new(exe)
        .env(HOLD_ENV, &socket)
        .env(SERVER_ENV, server_pid.to_string())
        .stdout(Stdio::from(server_in))
        .stderr(Stdio::from(client_out))
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("signal-rsh: failed to spawn the holder: {e}");
            exit(1);
        });
    let (link, _) = listener.accept().unwrap_or_else(|e| {
        eprintln!("signal-rsh: holder did not connect: {e}");
        exit(1);
    });
    let _ = std::fs::remove_file(&socket);
    link
}

fn main() {
    #[cfg(unix)]
    if let Some(socket) = env::var_os(HOLD_ENV) {
        hold(&socket.to_string_lossy());
    }
    if let Err(e) = fast_io::force_blocking_stdio() {
        eprintln!("signal-rsh: cannot clear O_NONBLOCK on stdio: {e}");
        exit(1);
    }
    let (c2s_at, s2c_at) = offsets();

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
    let child_in = child.stdin.take().expect("stdin was piped");
    let child_out = child.stdout.take().expect("stdout was piped");

    // On Unix the holder relays c2s and reads its offset itself.
    #[cfg(unix)]
    let _holder = {
        let _ = c2s_at;
        spawn_holder(child_in, child.id())
    };
    // The c2s pump may stay parked on our stdin, which the client holds open
    // until we exit.
    #[cfg(not(unix))]
    {
        let fire_in = signaller(child.id().to_string());
        thread::spawn(move || pump(std::io::stdin().lock(), child_in, c2s_at, fire_in));
    }
    // The s2c pump runs on the main thread and ends at the server's stdout
    // EOF.
    pump(
        child_out,
        std::io::stdout().lock(),
        s2c_at,
        signaller(child.id().to_string()),
    );
    let status = child.wait().expect("wait on server child");
    exit(status.code().unwrap_or(255));
}
