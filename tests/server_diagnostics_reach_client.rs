//! A server's `--info`/`--debug` output must reach the client as `MSG_INFO`.
//!
//! Upstream routes every diagnostic through `rwrite()`. On a server, FINFO -
//! which every `--info` and `--debug` line is - falls through the stream
//! switch (unless `quiet`) into `send_msg(MSG_INFO, ...)`, so the client prints
//! the server's lines on its own stdout:
//!
//! ```c
//! /* log.c:344-346 */ case FINFO: if (quiet) return; break;
//! /* log.c:357-366 */ if (am_server && msgs2stderr != 1 && ...) {
//!                         ... if (send_msg(msg, buf, len, !is_utf8)) return; }
//! ```
//!
//! What the server emits is decided by the `-v` letters the client forwards
//! (`options.c:2801`) and, for a daemon, capped at the module's
//! `max verbosity` (`clientserver.c:1223` -> `options.c:546-572`, default 1 per
//! `daemon-parm.txt:51`). The client never forwards `--debug=`
//! (`options.c:3126-3130`); explicit server debug arrives only via `-M`.
//!
//! The marker lines are server-only: a push runs the receiver on the server
//! (`recv_file_name(...)`, `DEBUG_GTE(FLIST, 2)`, debug_verbosity row 3) and a
//! pull runs the sender there (`send_files(...)`, `DEBUG_GTE(SEND, 1)`, row 3).
//!
//! Skip condition (the test passes with a printed reason): loopback TCP is
//! unavailable for the daemon cases.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use test_support::ReapOnDrop;

const PUSH_MARKER: &str = "recv_file_name(a)";
const PULL_MARKER: &str = "send_files(";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn source_tree(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("src dir");
    fs::write(src.join("a"), b"hello\n").expect("write a");
    fs::write(src.join("b"), b"world\n").expect("write b");
    src
}

/// A remote shell that drops the host argument and runs the server locally.
fn local_shell(root: &Path) -> PathBuf {
    let shell = root.join("lsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
    shell
}

/// Runs the client and returns its stdout, asserting a clean exit.
fn run_client(args: &[String]) -> String {
    let output = Command::new(oc_binary())
        .args(args)
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "client failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn ssh_push(flags: &[&str]) -> String {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let src = source_tree(root);
    let shell = local_shell(root);
    let mut args: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
    args.extend([
        "-a".to_owned(),
        "-e".to_owned(),
        shell.display().to_string(),
        format!("--rsync-path={}", oc_binary().display()),
        format!("{}/", src.display()),
        format!("lh:{}/", root.join("dst").display()),
    ]);
    run_client(&args)
}

fn ssh_pull(flags: &[&str]) -> String {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let src = source_tree(root);
    let shell = local_shell(root);
    let mut args: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
    args.extend([
        "-a".to_owned(),
        "-e".to_owned(),
        shell.display().to_string(),
        format!("--rsync-path={}", oc_binary().display()),
        format!("lh:{}/", src.display()),
        format!("{}/", root.join("dst").display()),
    ]);
    run_client(&args)
}

/// `-vvv` on the client makes the server receiver's FLIST2 line appear on the
/// client's stdout; `-vv` (row 2) does not raise FLIST to 2, so the line stays
/// out and the `-vv` output keeps upstream's shape.
#[test]
fn ssh_push_server_debug_reaches_client_at_vvv_only() {
    assert!(ssh_push(&["-vvv"]).contains(PUSH_MARKER));
    assert!(!ssh_push(&["-vv"]).contains(PUSH_MARKER));
}

/// The pull direction runs the sender on the server; its SEND1 line arrives
/// the same way.
#[test]
fn ssh_pull_server_debug_reaches_client_at_vvv_only() {
    assert!(ssh_pull(&["-vvv"]).contains(PULL_MARKER));
    assert!(!ssh_pull(&["-vv"]).contains(PULL_MARKER));
}

/// A quiet client packs `q` (options.c:2804-2805) and the server drops its
/// FINFO lines before framing them (log.c:344-346).
#[test]
fn ssh_quiet_server_sends_no_info() {
    assert!(!ssh_push(&["-vvv", "-q"]).contains(PUSH_MARKER));
    assert!(!ssh_pull(&["-vvv", "-q"]).contains(PULL_MARKER));
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Runs one daemon push against a module with `max verbosity = max_verbosity`
/// and returns the client's stdout and the daemon log, or `None` without
/// loopback TCP.
fn daemon_push(max_verbosity: u8, flags: &[&str]) -> Option<(String, String)> {
    daemon_transfer(max_verbosity, flags, true)
}

/// Like [`daemon_push`], but pulls from the module, so the worker is the sender.
fn daemon_pull(max_verbosity: u8, flags: &[&str]) -> Option<(String, String)> {
    daemon_transfer(max_verbosity, flags, false)
}

fn daemon_transfer(max_verbosity: u8, flags: &[&str], push: bool) -> Option<(String, String)> {
    let port = free_port()?;
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let src = source_tree(root);
    let module = root.join("mod");
    fs::create_dir_all(&module).expect("module dir");
    let log = root.join("daemon.log");
    let conf = root.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\nuse chroot = no\nreverse lookup = no\nlog file = {log}\n\n\
             [m]\n\tpath = {module}\n\tread only = no\n\tmax verbosity = {max_verbosity}\n",
            log = log.display(),
            module = module.display(),
        ),
    )
    .expect("config");
    let _daemon = ReapOnDrop::new(
        Command::new(oc_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", conf.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut args: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
    args.push("-a".to_owned());
    if push {
        args.push(format!("{}/", src.display()));
        args.push(format!("rsync://127.0.0.1:{port}/m/"));
    } else {
        for name in ["a", "b"] {
            fs::copy(src.join(name), module.join(name)).expect("seed module");
        }
        args.push(format!("rsync://127.0.0.1:{port}/m/"));
        args.push(format!("{}/", root.join("dst").display()));
    }
    let stdout = run_client(&args);
    let log_text = fs::read_to_string(&log).unwrap_or_default();
    Some((stdout, log_text))
}

/// A module that allows `-vvv` frames the worker's debug line to the client,
/// and the daemon still logs it (log.c:312-332 `logit()` before the send).
#[test]
fn daemon_server_debug_reaches_client_when_max_verbosity_allows() {
    let Some((stdout, log)) = daemon_push(3, &["-vvv"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(stdout.contains(PUSH_MARKER), "client stdout:\n{stdout}");
    assert!(log.contains(PUSH_MARKER), "daemon log:\n{log}");
}

/// The default `max verbosity = 1` caps the worker below row 3, so `-vvv`
/// produces no server debug at all (clientserver.c:1223).
#[test]
fn default_max_verbosity_suppresses_server_debug() {
    let Some((stdout, _)) = daemon_push(1, &["-vvv"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(!stdout.contains(PUSH_MARKER), "client stdout:\n{stdout}");
}

/// `-M--debug=flist2` reaches the worker's argv, applies at USER priority and
/// is capped per flag: rows <= 1 hold no FLIST2, rows <= 4 do
/// (options.c:546-572).
#[test]
fn remote_option_debug_is_applied_and_capped_per_flag() {
    let Some((capped, _)) = daemon_push(1, &["-v", "-M--debug=flist2"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(!capped.contains(PUSH_MARKER), "capped stdout:\n{capped}");
    let Some((allowed, _)) = daemon_push(4, &["-v", "-M--debug=flist2"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(allowed.contains(PUSH_MARKER), "allowed stdout:\n{allowed}");
}

/// `--quiet` stops the daemon worker framing FINFO too.
#[test]
fn daemon_quiet_server_sends_no_info() {
    let Some((stdout, _)) = daemon_push(3, &["-vvv", "-q"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(!stdout.contains(PUSH_MARKER), "client stdout:\n{stdout}");
}

/// A pull runs the sender in the worker: its SEND1 line reaches the client
/// and, as for the receiver, the daemon log keeps it too.
#[test]
fn daemon_sender_debug_reaches_client_and_daemon_log() {
    let Some((stdout, log)) = daemon_pull(3, &["-vvv"]) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(stdout.contains(PULL_MARKER), "client stdout:\n{stdout}");
    assert!(log.contains(PULL_MARKER), "daemon log:\n{log}");
}
