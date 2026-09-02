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
//! This test pins that contract for the threaded model: client one drives a
//! genuine session error, and client two must still complete a real transfer
//! against the same listener afterwards.
//!
//! The provoked error is a non-UTF-8 request line. It has to be an error the
//! daemon still raises: a repeated `@RSYNCD:` banner does NOT, because
//! `start_daemon()` reads the version line exactly once and treats the next
//! line as the request whatever it contains (clientserver.c:1534-1538), so a
//! second banner is answered as an unknown module rather than refused as an
//! out-of-order transition. Driving the FSM edge again would be fatal to the
//! whole listener, which is precisely the shape a peer must not be able to
//! reach.

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

/// Allocates a free TCP port and hands the bound listener to the daemon so
/// there is no TOCTOU window between allocation and the daemon's own bind.
fn allocate_test_port() -> Option<(u16, TcpListener)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((port, listener))
}

/// Connects, reads the greeting, echoes the version banner, then sends a
/// request line that is not valid UTF-8. The daemon reads that line with
/// `BufRead::read_line` into a `String`, which fails with `InvalidData` -
/// a session error that is neither a broken pipe nor a reset, so
/// `report_session_failure` logs it instead of treating it as a normal close.
/// What the provoking client observed on its own socket.
///
/// The greeting is kept alongside the trailing lines because the assertion that
/// fails is about which line the daemon treated as the module request - and that
/// is unanswerable from the trailing lines alone.
struct ProvokerOutcome {
    /// The daemon's greeting line, as received.
    greeting: String,
    /// Everything the daemon sent after the request line. Must be empty: the
    /// session error drops the connection rather than answering it.
    trailing: Vec<String>,
}

fn run_provoker(port: u16, deadline: Instant) -> Result<ProvokerOutcome, String> {
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
    let greeting = greeting.trim_end_matches(['\r', '\n']).to_owned();
    if !greeting.starts_with("@RSYNCD:") {
        return Err(format!("unexpected greeting: {greeting:?}"));
    }

    stream
        .write_all(b"@RSYNCD: 32.0 md5 md4\n\xff\xfe\n")
        .map_err(|err| format!("send non-utf8 request line: {err}"))?;
    stream.flush().map_err(|err| format!("flush: {err}"))?;

    let mut rest = String::new();
    let _ = reader.read_to_string(&mut rest);
    Ok(ProvokerOutcome {
        greeting,
        trailing: rest.lines().map(str::to_owned).collect(),
    })
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

    // Non-vacuity: the first client has to have genuinely broken its session.
    // If the daemon ever starts answering a repeated banner cleanly this
    // fixture stops provoking anything, and the property above would pass
    // while proving nothing - so fail loudly here instead.
    let provoker_outcome = provoker.expect("provoker client failed before it could provoke");
    assert!(
        provoker_outcome.trailing.is_empty(),
        "first client must be dropped mid-session, not answered.\n\
         got trailing lines: {:?}\n\
         greeting the provoker received: {:?}\n\
         client_result: {client_result:?}\n\
         daemon_result: {daemon_result:?}\n\
         daemon log:\n{log_contents}",
        provoker_outcome.trailing,
        provoker_outcome.greeting,
    );
    let failure_lines: Vec<&str> = log_contents
        .lines()
        .filter(|line| line.contains("failed to serve legacy handshake"))
        .collect();
    assert_eq!(
        failure_lines.len(),
        1,
        "expected exactly one logged session failure for the first client; log:\n{log_contents}"
    );
    assert!(
        failure_lines[0].contains("UTF-8"),
        "the provoked failure must be the non-UTF-8 request line, not some\n         unrelated session error that would make this fixture prove nothing: {}",
        failure_lines[0]
    );
}
