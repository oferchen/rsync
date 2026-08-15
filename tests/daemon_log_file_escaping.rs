//! Peer-supplied text reaching the daemon's log file is escaped (CWE-117).
//!
//! Upstream escapes in the *sink*, not in any renderer: `logit()` is the only
//! function that writes `logfile_fp`, and it hands every line to
//! `filtered_fwrite(logfile_fp, buf, len, 0, 1, trailing)` (log.c:126-132).
//! Nothing that reaches the log file can skip it.
//!
//! oc-rsync used to escape in the CLI out-format renderer instead. That
//! covered `--log-file` on the client, and nothing else: the daemon writes its
//! log through an entirely different path and so recorded a raw `ESC` where
//! rsync 3.5.0 writes `\#033`. This test pins the daemon half, which is the
//! half a renderer-side escaper cannot reach by construction.
//!
//! The vector is the client's argument vector, logged verbatim at
//! `module '%s' from %s: client args: ...` - it carries an operand the peer
//! chose, before the module has resolved anything, so no fixture can launder
//! it.
//!
//! Skip condition (test passes with a printed reason): loopback TCP is
//! unavailable, or the daemon does not answer.

#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A requested path carrying the two bytes that forge log output: `ESC`, which
/// starts a terminal control sequence, and `LF`, which would otherwise open a
/// second - fully prefix-stamped, therefore authentic-looking - log record.
const HOSTILE_OPERAND: &str = "A\x1bB\nC";

/// The same operand as the log file must render it. 0x1B is octal 033, 0x0A is
/// octal 012; every other byte is ASCII-printable and survives verbatim.
const ESCAPED_OPERAND: &str = "A\\#033B\\#012C";

/// A wholly printable operand, used as the run's non-vacuity control: it must
/// reach the log unchanged, so a sink that mangled or dropped every operand
/// could not satisfy both assertions.
const PLAIN_OPERAND: &str = "plain-name";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Starts a daemon and waits until its port answers, so the client never races
/// the listener.
fn spawn_daemon(conf: &Path, port: u16) -> Option<Child> {
    let mut child = Command::new(oc_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(child);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Pulls `operand` from the module. The file does not exist, so the transfer
/// fails - the log line under test is written before the lookup, and a failing
/// pull keeps the fixture from having to create a file whose name the host
/// filesystem may refuse.
fn request(port: u16, operand: &str, dest: &Path) {
    let _ = Command::new(oc_binary())
        .arg("-q")
        .arg(format!("rsync://127.0.0.1:{port}/m/{operand}"))
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run client");
}

#[test]
fn daemon_log_file_escapes_a_peer_supplied_operand() {
    let Some(port) = free_port() else {
        println!("skipping: loopback TCP unavailable");
        return;
    };
    let root = tempfile::tempdir().expect("temp dir");
    let log = root.path().join("daemon.log");
    let module = root.path().join("mod");
    fs::create_dir_all(&module).expect("module dir");
    let conf = root.path().join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\n\
             use chroot = no\n\
             log file = {log}\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = yes\n",
            log = log.display(),
            module = module.display(),
        ),
    )
    .expect("write config");

    let Some(mut daemon) = spawn_daemon(&conf, port) else {
        println!("skipping: daemon did not answer on 127.0.0.1:{port}");
        return;
    };
    let dest = root.path().join("dest");
    request(port, HOSTILE_OPERAND, &dest);
    request(port, PLAIN_OPERAND, &dest);
    let _ = daemon.kill();
    let _ = daemon.wait();

    let logged = fs::read(&log).expect("read daemon log");
    let text = String::from_utf8_lossy(&logged);
    assert!(
        text.contains(PLAIN_OPERAND),
        "the control operand never reached the log, so the assertions below \
         would be vacuous:\n{text}"
    );
    assert!(
        !logged.contains(&0x1b),
        "a raw ESC reached the daemon log file:\n{text}"
    );
    assert!(
        text.contains(ESCAPED_OPERAND),
        "expected {ESCAPED_OPERAND:?} in the daemon log:\n{text}"
    );
}
