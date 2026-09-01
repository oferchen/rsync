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
use std::sync::{Barrier, Mutex};
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

/// Why a client probe produced no outcome.
///
/// The two variants are kept apart because only one of them may be tolerated.
/// A probe that never reached the listener says nothing about the cap, while a
/// probe that connected and then failed is indistinguishable from a broken cap
/// and must fail the test - collapsing both into one string is what let a
/// "skip when every client failed" guard swallow real daemon breakage.
enum ClientError {
    /// The daemon never became connectable within the deadline.
    Connect(String),
    /// The client reached the daemon and the exchange then failed.
    Exchange(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(message) | Self::Exchange(message) => formatter.write_str(message),
        }
    }
}

/// Renders a client-error list for an assertion message.
fn describe(errors: &[ClientError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Connects to the daemon, waiting until it has bound or the deadline passes.
fn connect_to_daemon(port: u16, ready_deadline: Instant) -> Result<TcpStream, ClientError> {
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    loop {
        match TcpStream::connect_timeout(&target, Duration::from_millis(500)) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < ready_deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(ClientError::Connect(format!("connect: {err}"))),
        }
    }
}

/// Runs the daemon handshake on an established connection and classifies the
/// daemon's answer to the module request.
fn request_module(stream: &TcpStream) -> Result<ClientOutcome, ClientError> {
    let exchange =
        |context: &str, err: std::io::Error| ClientError::Exchange(format!("{context}: {err}"));

    let mut writer = stream
        .try_clone()
        .map_err(|err| exchange("clone stream", err))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| exchange("set_read_timeout", err))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| exchange("set_write_timeout", err))?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|err| exchange("clone stream", err))?,
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| exchange("read greeting", err))?;
    if !line.starts_with("@RSYNCD:") {
        return Err(ClientError::Exchange(format!(
            "unexpected greeting: {line:?}"
        )));
    }

    // The digest name list is mandatory for protocol > 31: a greeting with no
    // space after the version is refused with `@ERROR: your client omitted the
    // digest name list`. Sending a bare `@RSYNCD: 32.0` turns every probe into
    // a protocol refusal, so the module request - and the cap under test - is
    // never reached.
    //
    // upstream: rsync-3.5.0/clientserver.c:228-238 `exchange_protocols()`.
    writer
        .write_all(b"@RSYNCD: 32.0 md5 md4\n")
        .map_err(|err| exchange("send handshake", err))?;
    writer
        .write_all(format!("{MODULE_NAME}\n").as_bytes())
        .map_err(|err| exchange("send module", err))?;
    writer
        .flush()
        .map_err(|err| exchange("flush module", err))?;

    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|err| exchange("read response", err))?;
    let trimmed = line.trim_end_matches(['\r', '\n']).to_string();

    if trimmed.starts_with("@ERROR:") {
        // Drain the @RSYNCD: EXIT trailer the daemon sends after errors,
        // mirroring upstream's clientserver.c framing.
        let mut exit = String::new();
        let _ = reader.read_line(&mut exit);
        return Ok(ClientOutcome::Refused(trimmed));
    }

    if !trimmed.starts_with("@RSYNCD: OK") {
        return Err(ClientError::Exchange(format!(
            "unexpected module response: {trimmed:?}"
        )));
    }

    Ok(ClientOutcome::Admitted)
}

/// Runs one client probe against the capped module.
///
/// Every client - admitted, refused or failed - waits on `all_classified`
/// before returning, and returning is what drops the socket and releases any
/// slot the daemon granted. That makes the contended probe deterministic: no
/// slot can be handed back until all `TOTAL_CLIENTS` answers are in, so the
/// client that finds the cap saturated is guaranteed to exist regardless of
/// scheduling. The barrier replaces a fixed sleep, which only made the race
/// unlikely rather than impossible.
fn run_client(
    port: u16,
    ready_deadline: Instant,
    all_classified: &Barrier,
) -> Result<ClientOutcome, ClientError> {
    let stream = match connect_to_daemon(port, ready_deadline) {
        Ok(stream) => stream,
        Err(err) => {
            all_classified.wait();
            return Err(err);
        }
    };

    let outcome = request_module(&stream);
    all_classified.wait();
    outcome
}

/// Splits a daemon log line into its upstream prefix and its message.
///
/// Returns the message with `"YYYY/MM/DD HH:MM:SS [pid] "` removed, or `None`
/// when the line does not carry that prefix. Asserting on the message rather
/// than the whole line is what keeps a level check honest: every daemon log
/// line is stamped, so `line.starts_with("oc-rsync warning:")` can never hold
/// and a level assertion written that way cannot distinguish a warning from an
/// error.
///
/// upstream: rsync-3.5.0/log.c:135 `logit()` -
/// `fprintf(logfile_fp, "%s [%d] ", timestring(time(NULL)), (int)getpid())`.
fn daemon_log_message(line: &str) -> Option<&str> {
    let (date, rest) = line.split_once(' ')?;
    let (time, rest) = rest.split_once(' ')?;
    let (pid, message) = rest.split_once(' ')?;

    let digits_and = |text: &str, separator: char, groups: usize| {
        text.split(separator).count() == groups
            && text.chars().all(|c| c.is_ascii_digit() || c == separator)
    };
    (digits_and(date, '/', 3) && digits_and(time, ':', 3)).then_some(())?;
    pid.strip_prefix('[')?
        .strip_suffix(']')?
        .parse::<u32>()
        .ok()?;

    Some(message)
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
            // Mandatory. `RuntimeOptions::detach` defaults to true on Unix, so
            // `run_daemon` calls `become_daemon()`, whose fork parent is this
            // very test process and exits 0
            // (`platform::daemonize::become_daemon`). Without this the binary
            // terminates successfully before a single assertion below runs and
            // the harness records a pass: an unconditional `panic!` in this
            // function still reported PASSED.
            OsString::from("--no-detach"),
        ])
        .build();

    let daemon_handle = thread::spawn(move || run_daemon(config));

    // All clients are given the same deadline to wait for the daemon's
    // bind. If the bind never lands the test skips rather than failing.
    let ready_deadline = Instant::now() + Duration::from_secs(15);

    let all_classified = Barrier::new(TOTAL_CLIENTS);
    let outcomes: Vec<Result<ClientOutcome, ClientError>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..TOTAL_CLIENTS)
            .map(|_| scope.spawn(|| run_client(port, ready_deadline, &all_classified)))
            .collect();

        handles
            .into_iter()
            .map(|handle| handle.join().expect("client thread panicked"))
            .collect()
    });

    let mut admitted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut errors: Vec<ClientError> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(ClientOutcome::Admitted) => admitted += 1,
            Ok(ClientOutcome::Refused(payload)) => refused.push(payload),
            Err(err) => errors.push(err),
        }
    }

    // Skip only when no client could open a TCP connection at all, which means
    // the environment has no usable loopback rather than that the cap is
    // broken. A client that connected and then failed is a real failure and
    // falls through to the assertions below.
    if errors.len() == TOTAL_CLIENTS
        && errors
            .iter()
            .all(|err| matches!(err, ClientError::Connect(_)))
    {
        eprintln!(
            "daemon_caps_concurrent_module_connections_at_max_connections: skipped ({})",
            describe(&errors)
        );
        let _ = daemon_handle.join();
        return;
    }

    assert!(
        errors.is_empty(),
        "client errors during concurrent probe: {}",
        describe(&errors)
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

    // The daemon reaches `served >= max-sessions` once the third session ends,
    // then joins its workers and returns. Every client socket is closed by now
    // - `run_client` returns only after the barrier - so the join cannot block
    // on a connection this test still holds.
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
    let cap_message = daemon_log_message(cap_line).unwrap_or_else(|| {
        panic!("cap line must carry the upstream log prefix (log.c:135): {cap_line}")
    });
    assert!(
        cap_message.starts_with("oc-rsync warning:"),
        "cap line must be warning-level (DMC-5): {cap_line}"
    );
    assert!(
        cap_message.contains(&format!("which={MODULE_NAME}")),
        "missing which={MODULE_NAME}: {cap_line}"
    );
    assert!(
        cap_message.contains("(127.0.0.1)"),
        "missing peer ip field: {cap_line}"
    );
    assert!(
        cap_message.contains(&format!("cap={CONNECTION_CAP}")),
        "missing cap={CONNECTION_CAP}: {cap_line}"
    );
    assert!(
        cap_message.contains(&format!("current={CONNECTION_CAP}")),
        "missing current={CONNECTION_CAP}: {cap_line}"
    );
}
