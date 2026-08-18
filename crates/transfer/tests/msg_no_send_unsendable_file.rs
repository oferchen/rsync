//! Regression coverage for `MSG_NO_SEND` on the pipelined receiver.
//!
//! # Background
//!
//! When the sender cannot open a file it has already been asked for, it does
//! not answer the request. It emits `MSG_NO_SEND` carrying the file's index and
//! moves on to the next file (`sender.c:723`, plus `:669` for an over-long path
//! and `:751` for a diminished file under `--append`). Upstream's generator
//! retires the entry on receipt (`io.c:1809-1818` ->
//! `got_flist_entry_status(FES_NO_SEND, ndx)`), and its receiver is unaffected
//! because it is *NDX-addressed*: it reads whatever index arrives and looks the
//! file up by it (`rsync.c:322-431`).
//!
//! oc's pipelined receiver is instead *FIFO-positional* - it pops the oldest
//! outstanding request to interpret each response. A declined file therefore
//! has to be evicted from that window explicitly, or the request stays queued
//! forever waiting for a reply the sender already decided not to send.
//!
//! # Why this matters (Rule 9)
//!
//! The contract is: a source file the sender cannot open is skipped, the rest
//! of the transfer completes, and the run reports the I/O error. The failure
//! mode this pins is not a wrong byte - it is a **hang**. Two independent
//! defects produced it, and each alone is sufficient:
//!
//! 1. The response reader called the *combined* NDX+attributes helper, so on
//!    reaching `NDX_DONE` (a single `0x00` at protocol >= 30) it blocked in
//!    `read_exact` for two `iflags` bytes the peer never sends. Upstream
//!    returns at that point *before* consuming any attribute byte
//!    (`rsync.c:334-335`).
//! 2. The `MSG_NO_SEND` ledger was decoded and accumulated but never drained,
//!    so the declined file's request was never evicted from the in-flight
//!    window.
//!
//! Because either defect alone hangs the client, the test asserts on a
//! **bounded** run: a timeout is a failure, not an inconclusive result.
//!
//! # Fixture design
//!
//! The unreadable file is named to sort **last**. rsync transfers in sorted
//! order, so an unreadable file sorting first would be declined while the
//! window is still empty - the positional desync could not be observed and the
//! test would pass vacuously against both defects. Sorting it last guarantees
//! readable requests are outstanding when the decline arrives.
//!
//! # Skip semantics
//!
//! Self-skips when the binary or a loopback port is unavailable, and when the
//! running user can read a mode-000 file - which is the real precondition, so
//! it is probed behaviourally rather than inferred from a uid. A hang, a wrong
//! exit code, or a missing readable file are real regressions.
//!
//! # Upstream References
//!
//! - `sender.c:583-585` - non-transfer items are echoed and `continue`d
//!   *above* every `MSG_NO_SEND` emitter, so the message only ever names a
//!   transfer item.
//! - `sender.c:669,723,751` - the three emitters; each `continue`s without
//!   writing a response.
//! - `io.c:1809-1818` - `MSG_NO_SEND` -> `got_flist_entry_status(FES_NO_SEND)`.
//! - `sender.c:668,719` - `io_error |= IOERR_GENERAL`, which is what makes the
//!   run exit 23; the exit code travels via `MSG_IO_ERROR`, not `MSG_NO_SEND`.
//! - `rsync.c:334-335` - `NDX_DONE` returns before any attribute byte.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// Upper bound on the client run. The transfer moves a few kilobytes over
/// loopback; anything approaching this is the hang under test, not slowness.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Upstream `RERR_PARTIAL`: "some files could not be transferred".
const EXIT_PARTIAL_TRANSFER: i32 = 23;

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. `use chroot = false` keeps the unprivileged test process from
/// needing `CAP_SYS_CHROOT`.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [nosendmod]\n\
         path = {root}\n\
         comment = MSG_NO_SEND regression\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Kills the daemon child on drop so a panicking test never leaks the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Run one `oc-rsync` invocation under a deadline, returning `None` if it had
/// to be killed.
///
/// Both pipes are drained by dedicated threads *before* waiting. Polling
/// `try_wait()` while output accumulates in a pipe buffer is itself a hang: the
/// child blocks writing, the parent blocks waiting, and the deadline is the
/// only thing that ends it - which would report the hang under test even after
/// it is fixed.
fn run_under_deadline(bin: &Path, args: &[&OsStr]) -> io::Result<Option<(ExitStatus, String)>> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let out_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let combined = format!(
        "{}{}",
        out_reader.join().unwrap_or_default(),
        err_reader.join().unwrap_or_default()
    );
    Ok(status.map(|s| (s, combined)))
}

/// Per-test scratch state.
struct Scratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
}

impl Scratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        let root = tmp.path().to_path_buf();
        Some(Self {
            config: root.join("rsyncd.conf"),
            log: root.join("rsyncd.log"),
            pid: root.join("rsyncd.pid"),
            root,
            _tmp: tmp,
        })
    }
}

/// Whether this user is subject to file permissions at all.
///
/// Probed rather than derived from a uid: root reads a mode-000 file happily,
/// and so does any user holding `CAP_DAC_OVERRIDE`. The precondition the
/// fixture needs is exactly "opening this file fails", so that is what is
/// measured.
fn mode_000_is_unreadable(probe: &Path) -> bool {
    fs::write(probe, b"probe").is_ok()
        && fs::set_permissions(probe, fs::Permissions::from_mode(0o000)).is_ok()
        && fs::File::open(probe).is_err()
}

/// The three source names. `zz_unreadable.bin` sorts last so that requests for
/// the readable files are still outstanding when the decline arrives - see the
/// fixture note in the module docs.
const READABLE: [&str; 2] = ["a_first.bin", "b_second.bin"];
const UNREADABLE: &str = "zz_unreadable.bin";

/// Seed the module root. Every file gets a distinct size so a quick-check
/// cannot skip it on a re-run.
fn seed_module(module_root: &Path) {
    fs::create_dir_all(module_root).expect("create module root");
    for (i, name) in READABLE.iter().enumerate() {
        let body: Vec<u8> = (0..(4096 + i * 512)).map(|b| (b % 251) as u8).collect();
        fs::write(module_root.join(name), &body).expect("seed readable source");
    }
    let body: Vec<u8> = (0..8192).map(|b| (b % 241) as u8).collect();
    fs::write(module_root.join(UNREADABLE), &body).expect("seed unreadable source");
}

fn pull_args<'a>(src: &'a OsStr, dest: &'a OsStr) -> Vec<&'a OsStr> {
    vec![OsStr::new("--recursive"), OsStr::new("--times"), src, dest]
}

/// A source file the sender cannot open must be skipped, the rest of the
/// transfer must complete, and the run must report the I/O error - not hang.
#[test]
fn daemon_pull_skips_a_file_the_sender_cannot_open() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    if !mode_000_is_unreadable(&scratch.root.join("perm_probe")) {
        eprintln!("skipping: this user can read a mode-000 file (root or CAP_DAC_OVERRIDE)");
        return;
    }

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    seed_module(&module_root);
    fs::set_permissions(
        module_root.join(UNREADABLE),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make source unreadable");
    fs::create_dir_all(&dest_dir).expect("create dest");

    write_daemon_config(&scratch.config, &scratch.pid, &scratch.log, &module_root)
        .expect("write daemon config");
    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src = OsString::from(format!("rsync://127.0.0.1:{port}/nosendmod/"));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");
    let args = pull_args(&src, &dest_arg);

    let outcome = run_under_deadline(&oc_bin, &args).expect("spawn client");

    // The primary assertion. Before the fix the client blocked forever: either
    // in `read_exact` on NDX_DONE, or waiting on a response for the declined
    // file that the sender never sends.
    let Some((status, output)) = outcome else {
        panic!(
            "client did not exit within {RUN_TIMEOUT:?} - the receiver is waiting on a response \
             the sender declined to send (MSG_NO_SEND)"
        );
    };

    assert_eq!(
        status.code(),
        Some(EXIT_PARTIAL_TRANSFER),
        "unreadable source must exit {EXIT_PARTIAL_TRANSFER} (RERR_PARTIAL); output:\n{output}"
    );

    // The readable files must still arrive: a skip must cost exactly the one
    // file, not the remainder of the transfer.
    for name in READABLE {
        let landed = dest_dir.join(name);
        assert!(
            landed.is_file(),
            "readable source {name} must still transfer after the skip; output:\n{output}"
        );
        assert_eq!(
            fs::read(&landed).expect("read transferred file").len(),
            fs::metadata(module_root.join(name))
                .expect("stat source")
                .len() as usize,
            "{name} transferred with the wrong length"
        );
    }

    assert!(
        !dest_dir.join(UNREADABLE).exists(),
        "the file the sender could not open must not be created at the destination; \
         output:\n{output}"
    );
}

/// Non-vacuity companion: the identical fixture with every file readable must
/// transfer all three and exit 0.
///
/// Without this, the pin above would also pass if the module were simply
/// unreachable, the daemon refused every file, or the fixture could not
/// transfer anything at all.
#[test]
fn daemon_pull_transfers_every_file_when_all_are_readable() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    seed_module(&module_root);
    fs::create_dir_all(&dest_dir).expect("create dest");

    write_daemon_config(&scratch.config, &scratch.pid, &scratch.log, &module_root)
        .expect("write daemon config");
    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src = OsString::from(format!("rsync://127.0.0.1:{port}/nosendmod/"));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");
    let args = pull_args(&src, &dest_arg);

    let outcome = run_under_deadline(&oc_bin, &args).expect("spawn client");
    let Some((status, output)) = outcome else {
        panic!("client did not exit within {RUN_TIMEOUT:?} on the all-readable fixture");
    };

    assert_eq!(
        status.code(),
        Some(0),
        "all-readable fixture must exit 0; output:\n{output}"
    );
    for name in READABLE.iter().chain(std::iter::once(&UNREADABLE)) {
        assert!(
            dest_dir.join(name).is_file(),
            "{name} must transfer when readable; output:\n{output}"
        );
    }
}
