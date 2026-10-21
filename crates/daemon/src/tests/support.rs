#[allow(unused_imports)]
use super::*;
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(unix))]
use std::thread;
use std::time::Duration;
#[cfg(not(unix))]
use std::time::Instant;

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
        unresolved_id: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        use_chroot_explicit: false,
        max_connections: MaxConnections::Unlimited,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        insecure_links: false,
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
        zero_copy_policy: fast_io::ZeroCopyPolicy::Auto,
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
        unresolved_id: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        use_chroot_explicit: false,
        max_connections: MaxConnections::Unlimited,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        insecure_links: false,
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
        zero_copy_policy: fast_io::ZeroCopyPolicy::Auto,
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
/// upstream: clientserver.c:203-215 exchange_protocols() - the daemon reads the client's banner
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
/// upstream: clientserver.c:1779-1782 daemon_main() - `become_daemon()` unless
/// `--no-detach`.
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

/// Starts a test daemon on an already-listening socket, guaranteed to stay in
/// the foreground.
///
/// This is the only supported way for a test to run [`run_daemon`]; running it
/// directly reintroduces the silent-pass defect described on
/// [`force_no_detach`]. On Unix the daemon runs in its own process (see
/// `DaemonHandle`), so call this before opening any client socket.
///
/// The listener is bound and listening before the daemon starts, so a client
/// may connect at once: the kernel queues the connection until the daemon
/// accepts it. That is the whole readiness protocol - there is no window in
/// which the port is unbound.
pub(super) fn spawn_daemon(config: crate::DaemonConfig, held_listener: TcpListener) -> DaemonHandle {
    DaemonHandle::spawn(with_pre_bound_listener(
        force_no_detach(config),
        held_listener,
    ))
}

/// Starts a test daemon (see [`spawn_daemon`]) and connects one client to it.
pub(super) fn start_daemon(
    config: crate::DaemonConfig,
    port: u16,
    held_listener: TcpListener,
) -> (TcpStream, DaemonHandle) {
    let handle = spawn_daemon(config, held_listener);
    (connect_client(port), handle)
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

/// Waits for a test daemon to end and returns what it reported.
///
/// On Unix this blocks until the daemon process exits, which a `--once` or
/// `--max-sessions` daemon does after serving its sessions; it is always
/// `Some`. A daemon that crashed instead of returning fails the test.
///
/// Windows serves the daemon on a thread whose accept loop can linger past the
/// client's disconnect, so there the wait is bounded and a lingering daemon is
/// detached, returning `None`; nextest runs each test in its own process, so
/// the detached thread ends with it.
pub(super) fn finish_daemon(handle: DaemonHandle) -> Option<DaemonResult> {
    #[cfg(not(unix))]
    {
        const DAEMON_JOIN_TIMEOUT: Duration = Duration::from_secs(20);
        const POLL_INTERVAL: Duration = Duration::from_millis(50);

        let deadline = Instant::now() + DAEMON_JOIN_TIMEOUT;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
    Some(handle.join().unwrap_or_else(|crash| panic!("{crash}")))
}

/// Bounded read/write timeout applied to every daemon-test client stream.
///
/// Test clients drive the daemon with blocking `read_line`/`write_all` calls.
/// Without a timeout a wedged daemon worker leaves the client blocked until
/// nextest's slow-timeout terminates it 360s later, which reads as a job-level
/// hang. A generous per-operation timeout turns that hang into a fast failure.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Connects a client to a daemon started by [`spawn_daemon`].
///
/// The daemon's listener is already listening, so the connect completes
/// against the kernel's backlog whether or not the daemon has reached
/// `accept` yet.
pub(super) fn connect_client(port: u16) -> TcpStream {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to test daemon");
    stream
        .set_read_timeout(Some(CLIENT_IO_TIMEOUT))
        .expect("set client read timeout");
    stream
        .set_write_timeout(Some(CLIENT_IO_TIMEOUT))
        .expect("set client write timeout");
    stream
}
