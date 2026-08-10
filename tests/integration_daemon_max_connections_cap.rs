//! DMC-2: N concurrent connections capped at `max connections = N`.
//!
//! Spawns an oc-rsync daemon with a single module configured with
//! `max connections = 2`, then opens 3 concurrent client connections
//! against the module. The test asserts:
//!
//! * Exactly 2 clients are admitted (receive `@RSYNCD: OK`).
//! * Exactly 1 client is refused with the upstream-compatible payload
//!   `@ERROR: max connections (2) reached -- try again later`
//!   (DMC-3, upstream `clientserver.c:752`).
//! * The daemon writes exactly one structured warning to its log file in
//!   the shape introduced by DMC-5:
//!   `max-connections cap reached: which=<module> peer=<host> (<ip>) cap=2 current=2`.
//!
//! Gated to Unix only because Windows daemon parity is not yet certified
//! and the cap-reached log path is exercised through Unix-tested daemon
//! integration paths.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use daemon::{DaemonConfig, run_daemon};
use tempfile::tempdir;

/// Serialise daemon-spawning tests in this binary so port + log files
/// remain predictable across nextest threads.
static TEST_LOCK: Mutex<()> = Mutex::new(());

const MODULE_NAME: &str = "capped";
const CONNECTION_CAP: u32 = 2;
const TOTAL_CLIENTS: usize = 3;

/// Bind to ephemeral port, capture it, then release for the daemon.
fn allocate_test_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Outcome of a single client probing the capped module.
#[derive(Debug)]
enum ClientOutcome {
    /// Client received `@RSYNCD: OK` for the module request.
    Admitted,
    /// Client received the cap-reached `@ERROR:` payload (DMC-3).
    Refused(String),
}

/// Runs the full daemon handshake for a single client, then reports whether
/// the module request was admitted or refused.
///
/// Admitted clients block on `release` so the caller can hold the
/// connection slot open until all probes complete, ensuring the third
/// client races against a full cap rather than against an already-released
/// slot.
fn run_client(
    port: u16,
    ready_deadline: Instant,
    release: mpsc::Receiver<()>,
) -> Result<ClientOutcome, String> {
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    // Retry the initial connect until the daemon is bound.
    let mut stream = loop {
        match TcpStream::connect_timeout(&target, Duration::from_millis(500)) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < ready_deadline => {
                thread::sleep(Duration::from_millis(10));
            }
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
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("read greeting: {err}"))?;
    if !line.starts_with("@RSYNCD:") {
        return Err(format!("unexpected greeting: {line:?}"));
    }

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .map_err(|err| format!("send handshake: {err}"))?;
    stream
        .write_all(format!("{MODULE_NAME}\n").as_bytes())
        .map_err(|err| format!("send module: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("flush module: {err}"))?;

    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("read response: {err}"))?;
    let trimmed = line.trim_end_matches(['\r', '\n']).to_string();

    if trimmed.starts_with("@ERROR:") {
        // Drain the @RSYNCD: EXIT trailer the daemon sends after errors,
        // mirroring upstream's clientserver.c framing.
        let mut exit = String::new();
        let _ = reader.read_line(&mut exit);
        return Ok(ClientOutcome::Refused(trimmed));
    }

    if !trimmed.starts_with("@RSYNCD: OK") {
        return Err(format!("unexpected module response: {trimmed:?}"));
    }

    // Admitted: hold the connection open until the caller signals release
    // so the contended cap probe lands while both slots are still occupied.
    let _ = release.recv();
    Ok(ClientOutcome::Admitted)
}

#[test]
fn daemon_caps_concurrent_module_connections_at_max_connections() {
    let _guard = TEST_LOCK.lock().expect("test lock poisoned");

    let Some(port) = allocate_test_port() else {
        eprintln!(
            "daemon_caps_concurrent_module_connections_at_max_connections: skipped, no free port"
        );
        return;
    };

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    let lock_dir = temp.path().join("locks");
    let log_path = temp.path().join("daemon.log");
    fs::create_dir(&module_dir).expect("create module dir");
    fs::create_dir(&lock_dir).expect("create lock dir");

    let config_path = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "lock file = {lock}/rsyncd.lock\n\n\
         [{name}]\n\
         path = {path}\n\
         max connections = {cap}\n\
         use chroot = false\n",
        lock = lock_dir.display(),
        name = MODULE_NAME,
        path = module_dir.display(),
        cap = CONNECTION_CAP,
    );
    fs::write(&config_path, config_content).expect("write rsyncd.conf");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--max-sessions"),
            OsString::from(TOTAL_CLIENTS.to_string()),
        ])
        .build();

    let daemon_handle = thread::spawn(move || run_daemon(config));

    // All clients are given the same deadline to wait for the daemon's
    // bind. If the bind never lands the test skips rather than failing.
    let ready_deadline = Instant::now() + Duration::from_secs(15);

    let outcomes: Vec<Result<ClientOutcome, String>> = thread::scope(|scope| {
        let mut senders = Vec::with_capacity(TOTAL_CLIENTS);
        let mut handles = Vec::with_capacity(TOTAL_CLIENTS);
        for _ in 0..TOTAL_CLIENTS {
            let (tx, rx) = mpsc::channel::<()>();
            senders.push(tx);
            handles.push(scope.spawn(move || run_client(port, ready_deadline, rx)));
        }

        // Allow the first two clients to reach the module-request stage
        // and acquire connection slots; the third client must arrive
        // while the cap is still saturated.
        thread::sleep(Duration::from_millis(250));
        drop(senders);

        handles
            .into_iter()
            .map(|h| h.join().expect("client thread panicked"))
            .collect()
    });

    let mut admitted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(ClientOutcome::Admitted) => admitted += 1,
            Ok(ClientOutcome::Refused(payload)) => refused.push(payload),
            Err(err) => errors.push(err),
        }
    }

    // Skip cleanly when the daemon never bound (e.g. sandboxed CI).
    if admitted == 0 && refused.is_empty() && !errors.is_empty() {
        eprintln!(
            "daemon_caps_concurrent_module_connections_at_max_connections: skipped ({errors:?})"
        );
        let _ = daemon_handle.join();
        return;
    }

    assert!(
        errors.is_empty(),
        "client errors during concurrent probe: {errors:?}"
    );
    assert_eq!(
        admitted, CONNECTION_CAP as usize,
        "exactly {CONNECTION_CAP} clients should be admitted, got {admitted}; refused={refused:?}"
    );
    assert_eq!(
        refused.len(),
        TOTAL_CLIENTS - CONNECTION_CAP as usize,
        "exactly one client should be refused; admitted={admitted} refused={refused:?}"
    );

    // DMC-3: refusal payload mirrors upstream clientserver.c:752 exactly.
    let expected_error =
        format!("@ERROR: max connections ({CONNECTION_CAP}) reached -- try again later");
    assert_eq!(
        refused[0], expected_error,
        "refusal payload must mirror upstream clientserver.c:752 (DMC-3)"
    );

    // Wait for the daemon to drain and flush its log before assertions.
    // The daemon reaches `served >= max-sessions` after the third spawn,
    // then joins its workers and returns.
    let drain_deadline = Instant::now() + Duration::from_secs(10);
    while !daemon_handle.is_finished() && Instant::now() < drain_deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = daemon_handle.join();

    let log_contents = fs::read_to_string(&log_path).unwrap_or_default();

    // DMC-5: exactly one structured warning line for the rejected peer.
    let cap_lines: Vec<&str> = log_contents
        .lines()
        .filter(|line| line.contains("max-connections cap reached"))
        .collect();
    assert_eq!(
        cap_lines.len(),
        1,
        "expected exactly one cap-reached log line; got {}: {log_contents}",
        cap_lines.len()
    );
    let cap_line = cap_lines[0];
    assert!(
        cap_line.starts_with("oc-rsync warning:"),
        "cap line must be warning-level (DMC-5): {cap_line}"
    );
    assert!(
        cap_line.contains(&format!("which={MODULE_NAME}")),
        "missing which={MODULE_NAME}: {cap_line}"
    );
    assert!(
        cap_line.contains("(127.0.0.1)"),
        "missing peer ip field: {cap_line}"
    );
    assert!(
        cap_line.contains(&format!("cap={CONNECTION_CAP}")),
        "missing cap={CONNECTION_CAP}: {cap_line}"
    );
    assert!(
        cap_line.contains(&format!("current={CONNECTION_CAP}")),
        "missing current={CONNECTION_CAP}: {cap_line}"
    );
}
