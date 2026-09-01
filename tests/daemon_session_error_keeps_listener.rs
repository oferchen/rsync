//! One malformed client must not take the daemon's accept loop down with it.
//!
//! oc-rsync serves each accepted connection on a thread, so a session error
//! travels back to the accept loop through a `JoinHandle`. Upstream serves
//! each connection in a forked child and the parent reaps it with
//! `waitpid(-1, NULL, WNOHANG)` (socket.c:679) - a NULL status pointer, so the
//! child's outcome is discarded outright. The parent's `while (1)` accept loop
//! (socket.c:724-778) has no error exit at all: `poll` failure, `accept`
//! failure and `fork` failure each `continue`. Only listener setup can end the
//! daemon (`exit_cleanup(RERR_SOCKETIO)` at socket.c:699 and socket.c:715).
//!
//! This test pins that contract for the threaded model: client one is refused
//! mid-handshake, and client two must still complete a real transfer against
//! the same listener afterwards.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use core::client::ClientConfig;
use daemon::{DaemonConfig, run_daemon};
use tempfile::tempdir;

const MODULE_NAME: &str = "uploads";

/// The version banner the provoker repeats. Sent twice, the second copy lands
/// where the daemon expects a module name.
const REPEATED_BANNER: &str = "@RSYNCD: 32.0 md5 md4";

/// Allocates a free TCP port and hands the bound listener to the daemon so
/// there is no TOCTOU window between allocation and the daemon's own bind.
fn allocate_test_port() -> Option<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((port, listener))
}

/// Connects, reads the greeting, then sends the `@RSYNCD:` version banner
/// twice. Upstream reads exactly one line after the version exchange and takes
/// it as the module name (clientserver.c:1538-1570), so the second banner is
/// refused as an unknown module rather than served. That refusal is what this
/// fixture provokes.
fn run_provoker(port: u16, deadline: Instant) -> Result<Vec<String>, String> {
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = loop {
        match TcpStream::connect_timeout(&target, Duration::from_millis(500)) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(err) => return Err(format!("connect: {err}")),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| format!("set_read_timeout: {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| format!("set_write_timeout: {err}"))?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|err| format!("clone stream: {err}"))?,
    );
    let mut greeting = String::new();
    reader
        .read_line(&mut greeting)
        .map_err(|err| format!("read greeting: {err}"))?;
    if !greeting.starts_with("@RSYNCD:") {
        return Err(format!("unexpected greeting: {greeting:?}"));
    }

    stream
        .write_all(format!("{REPEATED_BANNER}\n{REPEATED_BANNER}\n").as_bytes())
        .map_err(|err| format!("send repeated banner: {err}"))?;
    stream.flush().map_err(|err| format!("flush: {err}"))?;

    let mut rest = String::new();
    let _ = reader.read_to_string(&mut rest);
    Ok(rest.lines().map(str::to_owned).collect())
}

#[test]
fn session_error_does_not_stop_the_accept_loop() {
    let Some((port, held_listener)) = allocate_test_port() else {
        eprintln!("session_error_does_not_stop_the_accept_loop: skipped, no free port");
        return;
    };

    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    let log_path = temp.path().join("daemon.log");
    fs::create_dir(&src).expect("create src");
    fs::create_dir(&dst).expect("create dst");
    fs::write(src.join("payload.txt"), b"second client payload").expect("write payload");

    let config_path = temp.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[{name}]\npath = {dst}\nuse chroot = false\nread only = false\n",
            name = MODULE_NAME,
            dst = dst.display(),
        ),
    )
    .expect("write rsyncd.conf");

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--max-sessions"),
            OsString::from("2"),
            // `RuntimeOptions::detach` defaults to true on Unix: without this
            // the daemon forks and the parent of that fork is the test
            // process, which exits 0 before a single assertion runs.
            OsString::from("--no-detach"),
        ])
        .pre_bound_listener(held_listener)
        .build();

    let daemon_handle = thread::spawn(move || run_daemon(daemon_config));

    let deadline = Instant::now() + Duration::from_secs(15);
    let provoker = run_provoker(port, deadline);

    // The accept loop reaps finished workers on its next idle poll (50ms).
    thread::sleep(Duration::from_millis(500));

    let mut src_arg = src.clone().into_os_string();
    src_arg.push("/");
    let url = format!("rsync://127.0.0.1:{port}/{MODULE_NAME}/");
    let client_config = ClientConfig::builder()
        .transfer_args([src_arg, OsString::from(&url)])
        .recursive(true)
        .build();
    let client_result = core::client::run_client(client_config);

    let daemon_result = daemon_handle.join().expect("daemon thread panicked");
    let log_contents = fs::read_to_string(&log_path).unwrap_or_default();

    // The property: the second client's transfer completes. Asserted on the
    // landed bytes, not merely on the absence of a crash.
    assert!(
        client_result.is_ok(),
        "second client must complete after the first client's session error; \
         got {client_result:?}, daemon={daemon_result:?}, log:\n{log_contents}"
    );
    let landed = fs::read(dst.join("payload.txt")).unwrap_or_default();
    assert_eq!(
        landed, b"second client payload",
        "second client's file must land in the module directory"
    );
    assert!(
        daemon_result.is_ok(),
        "one client's session error must not fail the daemon: {daemon_result:?}"
    );

    // Non-vacuity: the first client has to have been REFUSED, not served. The
    // daemon protocol refuses by answering `@ERROR:` and closing, so a silent
    // drop is the wrong expectation - upstream answers this exact input with
    // `@ERROR: Unknown module '%s'` (clientserver.c:1570). Pinning the refusal
    // itself is what keeps the property above from passing vacuously: if the
    // daemon ever started serving this client, both assertions fire.
    let provoker_trailing = provoker.expect("provoker client failed before it could provoke");
    assert_eq!(
        provoker_trailing,
        vec![format!("@ERROR: Unknown module '{REPEATED_BANNER}'")],
        "first client must be refused, not served; log:\n{log_contents}"
    );
    let refusal_needle = format!("unknown module '{REPEATED_BANNER}' tried from");
    let refusal_lines: Vec<&str> = log_contents
        .lines()
        .filter(|line| line.contains(&refusal_needle))
        .collect();
    assert_eq!(
        refusal_lines.len(),
        1,
        "expected exactly one logged module refusal for the first client; log:\n{log_contents}"
    );
}
