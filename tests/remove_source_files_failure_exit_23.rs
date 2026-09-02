//! A failed `--remove-source-files` unlink must exit 23 in EVERY topology.
//!
//! ```c
//! /* sender.c:455-462 - successful_send() */
//!   failed:
//!         rsyserr(FERROR_XFER, errno, "sender failed to remove %s", fname);
//! /* log.c:337-338 - rwrite() */
//!         case FERROR_XFER:
//!                 got_xfer_error = 1;
//! /* cleanup.c:217-218 - exit_cleanup() */
//!         if (!code && got_xfer_error)
//!                 code = RERR_PARTIAL;
//! ```
//!
//! Upstream reacts to a `MSG_SUCCESS` frame the instant `read_a_msg()`
//! demultiplexes it (`io.c:1793-1807` calls `successful_send(val)` inline), so
//! the `FERROR_XFER` for a refused unlink is written wherever the sender
//! happens to be doing I/O - always while the peer is still reading. oc's
//! sender accumulates the confirmations and drains them in a batch instead, so
//! the position of that batch relative to the goodbye handshake decides whether
//! the client ever sees the message.
//!
//! With a single drain placed AFTER the goodbye, the two pulling topologies
//! reported success for a destructive-intent option that had not done its job:
//!
//! | topology   | before | after |
//! |------------|--------|-------|
//! | local copy | 23     | 23    |
//! | ssh push   | 23     | 23    |
//! | ssh pull   | **0**  | 23    |
//! | daemon pull| **0**  | 23    |
//!
//! Both zeros are the same defect: on a pull the sender is the remote, its
//! diagnostic has to cross the wire as `MSG_ERROR_XFER`, and by the time the
//! post-goodbye drain wrote it the client had already sent its final `NDX_DONE`
//! and stopped reading. The push and local cells never noticed because there
//! the failing sender IS the process whose exit code is observed.
//!
//! WHY THE FIXTURE IS EACCES AND NOT A REFUSED SYSCALL.
//!
//! The source's parent is `chmod 555`, so `unlinkat` fails `EACCES` on every
//! platform and in every feature set. A seccomp-refused `unlink` would produce
//! `EPERM` only on Linux `--all-features` builds, which would make a green run
//! elsewhere say nothing about the behaviour under test.
//!
//! Skip conditions (the test passes with a printed reason):
//! - Loopback TCP is unavailable (daemon cell only).

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// What one topology reported for the same refused unlink.
struct Attempt {
    exit_code: Option<i32>,
    stderr: String,
    delivered: bool,
    source_remains: bool,
}

/// Seeds `<root>/<cell>` with `src/sub/f.txt` under a write-denied parent plus
/// an empty `dst/`, and returns that cell root.
///
/// The file itself stays writable: it is the PARENT's missing write bit that
/// refuses the unlink, which is what makes the failure `EACCES` rather than a
/// property of the file.
fn seed(root: &Path, cell: &str) -> PathBuf {
    let dir = root.join(cell);
    let sub = dir.join("src/sub");
    fs::create_dir_all(&sub).expect("create source dir");
    fs::create_dir_all(dir.join("dst")).expect("create dest dir");
    fs::write(sub.join("f.txt"), b"payload\n").expect("seed source file");
    let mut perms = fs::metadata(&sub)
        .expect("stat source parent")
        .permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&sub, perms).expect("deny write on the source parent");
    dir
}

/// Restores the write bit so the temp dir can be torn down.
fn unseal(dir: &Path) {
    let sub = dir.join("src/sub");
    if let Ok(meta) = fs::metadata(&sub) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&sub, perms);
    }
}

fn observe(dir: &Path, output: std::process::Output) -> Attempt {
    Attempt {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        delivered: dir.join("dst/sub/f.txt").exists(),
        source_remains: dir.join("src/sub/f.txt").exists(),
    }
}

/// An SSH-shaped remote-shell shim: drop the options and the host placeholder,
/// then exec the server command locally. Gives a genuine two-process transfer
/// over a pipe pair without an SSH server.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    let body = "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         # $1 is the host placeholder; the server command follows it.\n\
         shift || true\n\
         exec \"$@\"\n";
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

fn local_copy(root: &Path) -> Attempt {
    let dir = seed(root, "local-copy");
    let output = Command::new(oc_binary())
        .args([
            "-r",
            "--remove-source-files",
            &format!("{}/", dir.join("src").display()),
            &format!("{}/", dir.join("dst").display()),
        ])
        .output()
        .expect("run local copy");
    let attempt = observe(&dir, output);
    unseal(&dir);
    attempt
}

/// `--rsync-path` forces a real external `--server` process, so the sender runs
/// in its own process and its diagnostic has to cross the pipe.
fn remote_shell(root: &Path, cell: &str, pull: bool) -> Attempt {
    let dir = seed(root, cell);
    let shim = write_rsh_shim(root);
    let binary = oc_binary();
    let src = format!("{}/", dir.join("src").display());
    let dst = format!("{}/", dir.join("dst").display());
    let (from, to) = if pull {
        (format!("localhost:{src}"), dst)
    } else {
        (src, format!("localhost:{dst}"))
    };
    let output = Command::new(&binary)
        .args(["-r", "--remove-source-files", "--rsh"])
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(&from)
        .arg(&to)
        .output()
        .expect("run remote-shell transfer");
    let attempt = observe(&dir, output);
    unseal(&dir);
    attempt
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Starts a daemon and waits until its port answers.
///
/// `stdin` is `/dev/null` deliberately: with an inherited terminal or pipe the
/// daemon takes its single-connection inetd path and never listens.
fn spawn_daemon(conf: &Path, port: u16) -> Child {
    let child = Command::new(oc_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// The module is deliberately NOT `read only`: upstream refuses a source
/// removal on a read-only module, so a read-only fixture would report the same
/// "source still there" as the bug and prove nothing.
fn daemon_pull(root: &Path) -> Option<Attempt> {
    let dir = seed(root, "daemon-pull");
    let port = free_port()?;
    let conf_path = dir.join("rsyncd.conf");
    fs::write(
        &conf_path,
        format!(
            "port = {port}\n\
             use chroot = no\n\
             log file = {log}\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = no\n",
            log = dir.join("daemon.log").display(),
            module = dir.join("src").display(),
        ),
    )
    .expect("write daemon config");

    let mut daemon = spawn_daemon(&conf_path, port);
    let output = Command::new(oc_binary())
        .args([
            "-r",
            "--remove-source-files",
            &format!("rsync://127.0.0.1:{port}/m/"),
            &format!("{}/", dir.join("dst").display()),
        ])
        .output()
        .expect("run daemon pull");
    // Observe BEFORE killing the daemon: it is the daemon that performs the
    // removal, so tearing it down first would guarantee the very absence of
    // work this test is trying to detect.
    let attempt = observe(&dir, output);
    let _ = daemon.kill();
    let _ = daemon.wait();
    unseal(&dir);
    Some(attempt)
}

/// Reports everything wrong with one cell, rather than stopping at the first
/// problem: a caller that aborts on the earliest failing topology hides whether
/// the rest of the table is intact, which is the whole point of running four.
///
/// The delivery and source-still-there checks come first because they are what
/// keeps the cell honest - without them "exit 23" could be reporting a transfer
/// that never ran, or an unlink the fixture failed to refuse.
fn faults(topology: &str, attempt: &Attempt) -> Vec<String> {
    let mut faults = Vec::new();
    if !attempt.delivered {
        faults.push(format!(
            "{topology}: the file did not reach the destination, so the \
             exit-code assertion would be meaningless"
        ));
    }
    if !attempt.source_remains {
        faults.push(format!(
            "{topology}: the source was removed, so the fixture did not exercise \
             a FAILED unlink and this cell proves nothing"
        ));
    }
    if !attempt.stderr.contains("sender failed to remove") {
        faults.push(format!(
            "{topology}: upstream's sender.c:455-459 text never reached the \
             observing process; on a pull that means it did not cross the wire \
             as MSG_ERROR_XFER while the client was still reading"
        ));
    }
    if attempt.exit_code != Some(23) {
        faults.push(format!(
            "{topology}: exit {:?}, want 23 - a refused source unlink is \
             RERR_PARTIAL (sender.c:455-459 -> log.c:337-338 -> cleanup.c:217-218)",
            attempt.exit_code
        ));
    }
    for fault in &mut faults {
        fault.push_str(&format!("\n    stderr: {:?}", attempt.stderr));
    }
    faults
}

/// All four topologies, one fixture, one assertion. Splitting them into
/// separate tests would let the two pulling cells be quietly dropped, which is
/// how the divergence survived: the local and push cells were green throughout.
#[test]
fn refused_source_unlink_exits_23_in_every_topology() {
    let root = tempfile::tempdir().expect("temp dir");

    let mut cells = vec![
        ("local copy", local_copy(root.path())),
        ("ssh push", remote_shell(root.path(), "ssh-push", false)),
        ("ssh pull", remote_shell(root.path(), "ssh-pull", true)),
    ];
    match daemon_pull(root.path()) {
        Some(attempt) => cells.push(("daemon pull", attempt)),
        None => println!("SKIP daemon pull: no loopback port available"),
    }

    let faults: Vec<String> = cells
        .iter()
        .flat_map(|(topology, attempt)| faults(topology, attempt))
        .collect();
    assert!(
        faults.is_empty(),
        "a refused --remove-source-files unlink must exit 23 everywhere:\n  {}",
        faults.join("\n  ")
    );
}
