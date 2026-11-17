//! A shutdown signal must stop a transfer whose writer is blocked on a full
//! transport, with exit code 20 and upstream's message.
//!
//! upstream: SIGINT, SIGTERM and SIGHUP share `sig_int` (main.c:1923-1925),
//! installed without `SA_RESTART`. A client sender exits from inside the
//! handler (rsync.c:855, rsync.c:874); a server records the signal and
//! `perform_io` acts on it at io.c:849-850 and io.c:997-998. Either way a
//! process blocked writing to a peer that no longer reads stops with
//! `RERR_SIGNAL`, "received SIGINT, SIGTERM, or SIGHUP" (log.c:102).
//!
//! oc used to retry the interrupted write (client) or the interrupted wait
//! (server) and stayed blocked until SIGKILL.
//!
//! The remote shell is `stall-rsh`, which runs the real server and stops
//! relaying one direction, so the side writing it fills its transport. The
//! signal is sent only once the writer is parked in the call under test, read
//! from `/proc/<tid>/syscall`, so it never arrives before the block:
//! - push: this test, the client's parent, waits for a client thread asleep
//!   in `write`, then signals the client;
//! - pull: the shim, the server's parent, waits for the server's stdout pipe
//!   to fill and a server thread to sleep in `poll`, then signals the server.
//!
//! Each stall is held until the signalled process exits, so neither can end
//! on end of file or `EPIPE` instead.
//!
//! Linux only, for `/proc/<tid>/syscall`. Other Unix systems are covered by
//! the unit tests of each part of the fix in `fast_io`, `rsync_io` and
//! `transfer`.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};

use test_support::deadline::{Deadlined, run_deadlined};

/// Bounds each run; a run that reaches it is the hang this test exists for.
const BUDGET: Duration = Duration::from_secs(60);

/// upstream log.c:102, the `RERR_SIGNAL` text.
const SIGNAL_TEXT: &str = "received SIGINT, SIGTERM, or SIGHUP (code 20)";

/// The syscall a sleeping thread is blocked in, or `None` while it runs or
/// once it has exited.
///
/// # Panics
///
/// Panics when `syscall` cannot be read for another reason: without ptrace
/// access to the thread (Yama) nothing here can be tested.
fn blocked_syscall(task: &Path) -> Option<String> {
    let stat = fs::read_to_string(task.join("stat")).ok()?;
    let state = stat.rsplit_once(')')?.1.split_whitespace().next()?;
    if state != "S" {
        return None;
    }
    let syscall = match fs::read_to_string(task.join("syscall")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("cannot read {}/syscall: {e}", task.display()),
    };
    let number = syscall.split_whitespace().next()?;
    (number != "running").then(|| number.to_owned())
}

/// The number `/proc/<tid>/syscall` shows for a thread blocked in a std
/// `write`, learned from a thread of ours blocked on a full pipe, so no
/// per-architecture table is needed.
fn write_syscall_number() -> String {
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    let (tx, rx) = mpsc::channel();
    let blocked = thread::spawn(move || {
        tx.send(fs::read_link("/proc/thread-self").expect("thread-self"))
            .expect("send");
        let chunk = [0u8; 65536];
        while writer.write(&chunk).is_ok() {}
    });
    let task = Path::new("/proc").join(rx.recv().expect("calibration thread"));
    let number = loop {
        if let Some(number) = blocked_syscall(&task) {
            break number;
        }
        thread::yield_now();
    };
    drop(reader);
    blocked.join().expect("calibration thread");
    number
}

/// Waits until the shim names the stalled client and a client thread sleeps
/// in `write`, then sends `signal`. Returns `false` if the client finished
/// first.
fn signal_blocked_client(
    status: &Path,
    signal: Signal,
    write_number: &str,
    client_done: &thread::JoinHandle<std::io::Result<Deadlined>>,
) -> bool {
    let client = loop {
        if client_done.is_finished() {
            return false;
        }
        let text = fs::read_to_string(status).unwrap_or_default();
        if let Some(pid) = text.lines().find_map(|line| line.strip_prefix("stalled ")) {
            break pid.parse::<i32>().expect("client pid");
        }
        thread::yield_now();
    };
    let tasks = Path::new("/proc").join(client.to_string()).join("task");
    loop {
        if client_done.is_finished() {
            return false;
        }
        let parked = fs::read_dir(&tasks)
            .into_iter()
            .flatten()
            .flatten()
            .any(|task| blocked_syscall(&task.path()).as_deref() == Some(write_number));
        if parked {
            break;
        }
        thread::yield_now();
    }
    let pid = Pid::from_raw(client).expect("client pid");
    kill_process(pid, signal).expect("signal the client");
    true
}

/// A file large enough that the stalled direction fills long before the
/// transfer could finish. Pseudo-random so compression cannot shrink it.
fn write_payload(path: &Path) {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let bytes: Vec<u8> = (0..8 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    fs::write(path, bytes).expect("write payload");
}

enum Direction {
    /// The client pushes; the shim stops reading the client's input.
    Push,
    /// The client pulls; the shim stops draining the server's output.
    Pull,
}

struct Outcome {
    client: Deadlined,
    status: String,
}

fn run(direction: &Direction, signal: &str) -> Outcome {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    write_payload(&src.join("big"));
    let status = root.join("status");

    let oc = test_support::oc_rsync_bin();
    let (from, to, stall) = match direction {
        Direction::Push => (
            format!("{}/", src.display()),
            format!("lh:{}/", dst.display()),
            "in",
        ),
        Direction::Pull => (
            format!("lh:{}/", src.display()),
            format!("{}/", dst.display()),
            "out",
        ),
    };
    let mut cmd = Command::new(&oc);
    cmd.arg("-a")
        .arg("-e")
        .arg(env!("CARGO_BIN_EXE_stall-rsh"))
        .arg("--rsync-path")
        .arg(&oc)
        .arg(from)
        .arg(to)
        .env("OC_STALL_RSH_DIR", stall)
        .env("OC_STALL_RSH_SIGNAL", signal)
        .env("OC_STALL_RSH_STATUS", &status);

    let write_number = write_syscall_number();
    let client_done = thread::spawn(move || run_deadlined(&mut cmd, BUDGET));
    let signalled = match direction {
        Direction::Push => {
            let signal = match signal {
                "INT" => Signal::INT,
                "TERM" => Signal::TERM,
                _ => Signal::HUP,
            };
            signal_blocked_client(&status, signal, &write_number, &client_done)
        }
        Direction::Pull => false,
    };
    let client = client_done
        .join()
        .expect("client thread")
        .expect("run client");
    let mut status = fs::read_to_string(&status).unwrap_or_default();
    if signalled {
        status.push_str("signalled\n");
    }
    Outcome { client, status }
}

fn assert_stopped_by(direction: &Direction, signal: &str) {
    let Outcome { client, status } = run(direction, signal);
    let (code, stderr) = match &client {
        Deadlined::Finished { status, stderr, .. } => {
            (status.code(), String::from_utf8_lossy(stderr).into_owned())
        }
        Deadlined::Expired { stderr, .. } => panic!(
            "SIG{signal}: the transfer did not stop within {BUDGET:?}; shim: {status:?}; stderr: {}",
            String::from_utf8_lossy(stderr)
        ),
    };
    assert!(
        !status.lines().any(|line| line.starts_with("error: ")),
        "SIG{signal}: the stall shim failed; shim: {status:?}; stderr: {stderr}"
    );
    assert!(
        status.lines().any(|line| line == "signalled"),
        "SIG{signal}: the shim never saw the writer blocked, so nothing was tested; \
         shim: {status:?}; stderr: {stderr}"
    );
    assert!(
        stderr.contains(SIGNAL_TEXT),
        "SIG{signal}: upstream's message is missing; stderr: {stderr}"
    );
    match direction {
        Direction::Push => assert_eq!(code, Some(20), "SIG{signal}: client exit; {stderr}"),
        Direction::Pull => assert!(
            status.lines().any(|line| line == "server_exit 20"),
            "SIG{signal}: the server must exit 20; shim: {status:?}; stderr: {stderr}"
        ),
    }
}

/// The pushing client is the sender; it blocks in a socket write.
#[test]
fn a_signal_stops_a_client_sender_blocked_in_a_write() {
    for signal in ["TERM", "INT", "HUP"] {
        assert_stopped_by(&Direction::Push, signal);
    }
}

/// The pulled-from server is the sender; it blocks waiting for room on its
/// stdout.
#[test]
fn a_signal_stops_a_server_sender_blocked_in_a_wait() {
    for signal in ["TERM", "INT", "HUP"] {
        assert_stopped_by(&Direction::Pull, signal);
    }
}
