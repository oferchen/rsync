#[allow(unused_imports)]
use super::*;
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::daemon::{
    HostPattern, MaxConnections, ModuleDefinition, TEST_SECRETS_CANDIDATES, TEST_SECRETS_ENV,
    TestSecretsEnvOverride,
};
pub(super) use crate::test_env::{ENV_LOCK, EnvGuard};
use core::branding;

pub(super) const RSYNCD: &str = branding::daemon_program_name();
pub(super) const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

pub(super) fn base_module(name: &str) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from(name),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        auth_digest: None,
        secrets_file: None,
        refuse_options: Vec::new(),
        read_only: true,
        write_only: false,
        numeric_ids: None,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        use_chroot_explicit: false,
        max_connections: MaxConnections::Unlimited,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        munge_symlinks: None,
        max_verbosity: 1,
        ignore_errors: false,
        ignore_nonreadable: false,
        transfer_logging: false,
        log_format: Some("%o %h [%a] %m (%u) %f %l".to_owned()),
        log_file: None,
        dont_compress: None,
        early_exec: None,
        pre_xfer_exec: None,
        post_xfer_exec: None,
        name_converter: None,
        temp_dir: None,
        charset: None,
        forward_lookup: true,
        strict_modes: true,
        exclude_from: None,
        include_from: None,
        open_noatime: false,
        reverse_lookup: true,
        lock_file: None,
        syslog_tag: None,
        syslog_facility: None,
        filter: Vec::new(),
        exclude: Vec::new(),
        include: Vec::new(),
    }
}

pub(super) fn module_with_host_patterns(allow: &[&str], deny: &[&str]) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from("module"),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: allow
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse allow pattern"))
            .collect(),
        hosts_deny: deny
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse deny pattern"))
            .collect(),
        auth_users: Vec::new(),
        auth_digest: None,
        secrets_file: None,
        refuse_options: Vec::new(),
        read_only: true,
        write_only: false,
        numeric_ids: None,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        use_chroot_explicit: false,
        max_connections: MaxConnections::Unlimited,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        munge_symlinks: None,
        max_verbosity: 1,
        ignore_errors: false,
        ignore_nonreadable: false,
        transfer_logging: false,
        log_format: Some("%o %h [%a] %m (%u) %f %l".to_owned()),
        log_file: None,
        dont_compress: None,
        early_exec: None,
        pre_xfer_exec: None,
        post_xfer_exec: None,
        name_converter: None,
        temp_dir: None,
        charset: None,
        forward_lookup: true,
        strict_modes: true,
        exclude_from: None,
        include_from: None,
        open_noatime: false,
        reverse_lookup: true,
        lock_file: None,
        syslog_tag: None,
        syslog_facility: None,
        filter: Vec::new(),
        exclude: Vec::new(),
        include: Vec::new(),
    }
}

pub(super) fn with_test_secrets_candidates<F, R>(candidates: Vec<PathBuf>, func: F) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_CANDIDATES.with(|cell| {
        let previous = cell.replace(Some(candidates));
        let result = func();
        cell.replace(previous);
        result
    })
}

pub(super) fn with_test_secrets_env<F, R>(
    override_value: Option<TestSecretsEnvOverride>,
    func: F,
) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_ENV.with(|cell| {
        let previous = cell.replace(override_value);
        let result = func();
        cell.replace(previous);
        result
    })
}

/// Allocates a free TCP port for daemon tests.
///
/// Binds to port 0 (OS-assigned ephemeral port) to guarantee no collision
/// at allocation time. The listener is kept alive and returned alongside the
/// port so the caller can hold it until the daemon is ready to bind,
/// minimizing the TOCTOU window between release and daemon bind.
pub(super) fn allocate_test_port() -> (u16, TcpListener) {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port for test");
    let port = listener.local_addr().expect("local addr").port();
    (port, listener)
}

/// Sends the client's `@RSYNCD:` version banner, completing the half of the
/// handshake a test performs before it issues a request.
///
/// upstream: clientserver.c:180-184 - the daemon reads the client's banner
/// first and answers `@ERROR: protocol startup error` when the line is not one,
/// so a request that arrives before the banner is refused rather than served.
/// Verified against rsync 3.4.4 for `#list`, a bare module name, an empty
/// (list-all) line, and an HTTP probe.
///
/// Tests that deliberately send something else in the banner's place (the
/// startup-error and panic-isolation cases) write it themselves instead.
pub(super) fn send_client_greeting(stream: &mut TcpStream) {
    stream
        .write_all(legacy_daemon_greeting().as_bytes())
        .expect("send client greeting");
}

pub(super) fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(args, &mut stdout, &mut stderr);
    (code, stdout, stderr)
}

#[cfg(unix)]
#[allow(dead_code)] // Used by disabled fallback tests
pub(super) fn write_executable_script(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set script permissions");
}

/// Forces a test-started daemon to stay in the foreground.
///
/// `RuntimeOptions::detach` defaults to `true` on Unix
/// (`daemon/runtime_options/types.rs`), so a daemon started with the arguments
/// a test typically supplies calls `become_daemon()`, which forks. The parent
/// of that fork is the test process itself, and it exits with status 0
/// (`platform::daemonize::become_daemon`). The test binary therefore terminates
/// successfully before a single assertion runs and the harness records a pass -
/// an unconditional `assert!(false)` in such a test still reports PASSED.
///
/// Appending `--no-detach` after the caller's arguments makes that impossible:
/// detach flags are applied in argument order and the last one wins
/// (`daemon/runtime_options/parsing.rs`), so no caller-supplied argument can
/// re-enable detaching. A caller that asks for `--detach` outright is rejected
/// loudly rather than silently corrected, because that request can only be a
/// mistake in a test.
///
/// upstream: clientserver.c:1513 - `become_daemon()`
fn force_no_detach(config: crate::DaemonConfig) -> crate::DaemonConfig {
    assert!(
        !config
            .arguments()
            .iter()
            .any(|argument| argument == "--detach"),
        "daemon tests must never pass --detach: become_daemon() forks and the \
         parent - this test process - exits 0 before any assertion runs, so the \
         test would report a pass without proving anything"
    );

    let mut arguments = config.arguments().to_vec();
    arguments.push(OsString::from("--no-detach"));
    crate::DaemonConfigBuilder::from(config)
        .arguments(arguments)
        .build()
}

/// Spawns an in-process daemon thread that is guaranteed to stay in the
/// foreground.
///
/// This is the only supported way for a test to run [`run_daemon`] on a thread;
/// spawning it directly reintroduces the silent-pass defect described on
/// [`force_no_detach`].
pub(super) fn spawn_daemon(
    config: crate::DaemonConfig,
) -> thread::JoinHandle<Result<(), crate::DaemonError>> {
    spawn_daemon_thread(force_no_detach(config))
}

/// Spawns a daemon thread without forcing `--no-detach`.
///
/// Temporary: the tests still calling this predate the foreground requirement
/// and are vacuous on Unix for the reason described on [`force_no_detach`].
/// They are migrated to [`spawn_daemon`] in separate slices so the assertions
/// they start executing can be triaged a few at a time. Do not add call sites -
/// `tools/ci/check_daemon_no_detach.sh` fails when the population grows.
pub(super) fn spawn_daemon_pending_no_detach(
    config: crate::DaemonConfig,
) -> thread::JoinHandle<Result<(), crate::DaemonError>> {
    spawn_daemon_thread(config)
}

fn spawn_daemon_thread(
    config: crate::DaemonConfig,
) -> thread::JoinHandle<Result<(), crate::DaemonError>> {
    thread::spawn(move || run_daemon(config))
}

/// Spawn a daemon thread and connect, failing fast if the daemon exits early.
///
/// Returns the connected stream and the daemon thread handle.  If the daemon
/// thread finishes before a connection succeeds the actual daemon error is
/// included in the panic message instead of waiting for the full 30-second
/// timeout.
///
/// The pre-bound `TcpListener` is passed directly to the daemon via
/// `DaemonConfig`, eliminating the TOCTOU race between port allocation and
/// daemon bind that previously caused flaky failures under CI load.
///
/// The daemon is forced into the foreground by [`force_no_detach`], so a caller
/// that omits `--no-detach` still gets a test that actually runs its
/// assertions.
pub(super) fn start_daemon(
    config: crate::DaemonConfig,
    port: u16,
    held_listener: TcpListener,
) -> (
    TcpStream,
    thread::JoinHandle<Result<(), crate::DaemonError>>,
) {
    connect_started_daemon(
        spawn_daemon(with_pre_bound_listener(config, held_listener)),
        port,
    )
}

/// [`start_daemon`] without the foreground guarantee.
///
/// Temporary, for the same reason and with the same migration plan as
/// [`spawn_daemon_pending_no_detach`]. Every call site is a test whose
/// assertions do not run on Unix; Windows is the only platform where they do,
/// because `become_daemon()` is `#[cfg(unix)]` and `detach` defaults to
/// `cfg!(unix)`. These tests deliberately stay enabled on Windows: the daemon
/// is supported there, and until the migration lands Windows is the only
/// platform whose result means anything.
pub(super) fn start_daemon_pending_no_detach(
    config: crate::DaemonConfig,
    port: u16,
    held_listener: TcpListener,
) -> (
    TcpStream,
    thread::JoinHandle<Result<(), crate::DaemonError>>,
) {
    connect_started_daemon(
        spawn_daemon_pending_no_detach(with_pre_bound_listener(config, held_listener)),
        port,
    )
}

/// Injects an already-bound listener so the daemon reuses it instead of binding
/// a new socket to the same port.
fn with_pre_bound_listener(
    config: crate::DaemonConfig,
    held_listener: TcpListener,
) -> crate::DaemonConfig {
    crate::DaemonConfigBuilder::from(config)
        .pre_bound_listener(held_listener)
        .build()
}

fn connect_started_daemon(
    handle: thread::JoinHandle<Result<(), crate::DaemonError>>,
    port: u16,
) -> (
    TcpStream,
    thread::JoinHandle<Result<(), crate::DaemonError>>,
) {
    let stream = connect_to_daemon(port, Some(&handle));
    (stream, handle)
}

pub(super) fn connect_with_retries(port: u16) -> TcpStream {
    connect_to_daemon(port, None)
}

/// Bounded wait for a daemon thread to finish, detaching it if it lingers.
///
/// The negotiation tests spawn an in-process daemon and then `join()` its
/// thread to surface the daemon-side `Result`. That thread's accept loop
/// terminates promptly on Unix once the client disconnects, but on Windows a
/// blocking `accept` on a re-bound listener can linger indefinitely, so an
/// unconditional `join()` wedges the test until nextest's 360s slow-timeout
/// fires - a job-level hang. Because nextest runs each test in its own process,
/// a still-running daemon thread is harmless: it is reaped when the test
/// process exits. This helper waits up to `DAEMON_JOIN_TIMEOUT` for the thread
/// to finish and returns its `Result`; if it does not finish it detaches the
/// thread and returns `None`, letting the caller keep the client-side
/// assertions that already validated the exchange.
pub(super) fn finish_daemon(
    handle: thread::JoinHandle<Result<(), crate::DaemonError>>,
) -> Option<Result<(), crate::DaemonError>> {
    const DAEMON_JOIN_TIMEOUT: Duration = Duration::from_secs(20);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    let deadline = Instant::now() + DAEMON_JOIN_TIMEOUT;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return Some(handle.join().expect("daemon thread"));
        }
        thread::sleep(POLL_INTERVAL);
    }
    None
}

/// Bounded read/write timeout applied to every daemon-test client stream.
///
/// Test clients drive the daemon with blocking `read_line`/`write_all` calls.
/// Without a timeout a wedged daemon worker - e.g. an accept-loop stall seen
/// under heavy concurrent load on Windows CI - leaves the client blocked until
/// nextest's slow-timeout terminates it 360s later, which reads as a job-level
/// hang. A generous per-operation timeout turns that hang into a fast,
/// retryable failure while staying clear of legitimate startup latency on a
/// loaded runner.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(30);

fn connect_to_daemon(
    port: u16,
    handle: Option<&thread::JoinHandle<Result<(), crate::DaemonError>>>,
) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    // CI environments may have resource constraints causing slower daemon startup
    const TIMEOUT: Duration = Duration::from_secs(30);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        // Fail fast if the daemon thread has already exited.
        if let Some(h) = handle {
            if h.is_finished() {
                panic!(
                    "daemon exited before accepting a connection on port {port}; \
                     check daemon startup logs for bind errors or config issues"
                );
            }
        }

        match TcpStream::connect_timeout(&target, backoff) {
            Ok(stream) => {
                // Bound every subsequent blocking read/write so a stalled
                // daemon worker fails fast and retries instead of hanging.
                stream
                    .set_read_timeout(Some(CLIENT_IO_TIMEOUT))
                    .expect("set client read timeout");
                stream
                    .set_write_timeout(Some(CLIENT_IO_TIMEOUT))
                    .expect("set client write timeout");
                return stream;
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    panic!("failed to connect to daemon within timeout: {error}");
                }

                thread::sleep(backoff);
                backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
            }
        }
    }
}
