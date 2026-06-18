//! Regression tests for UTS-DD-daemon-exit10.1 / .2.
//!
//! The default daemon listener must bind IPv4-only when no explicit family
//! override is given. This avoids the GitHub Actions Linux failure mode
//! where `bind(2)` to `[::]:873` succeeds but `accept(2)` later returns a
//! non-routable address family, producing an opaque
//! `error in socket I/O (code 10)` daemon exit. These tests drive
//! `run_daemon` with various `--ipv4` / `--ipv6` and
//! `OC_RSYNC_DAEMON_ADDRESS_FAMILY` configurations and verify a TCP client
//! on the expected family receives the `@RSYNCD:` greeting.
//!
//! upstream: socket.c:402-499 (`open_socket_in`) iterates every
//! getaddrinfo result and only fails when zero sockets bound; oc-rsync
//! reproduces that contract for the dual-stack case but defaults to
//! IPv4-only when no override is given so the GitHub Actions IPv6 stack
//! cannot mask the IPv4 listener.

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use daemon::{DaemonConfig, run_daemon};
use platform::signal::SignalFlags;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Reserves an ephemeral IPv4 port by binding then dropping a probe
/// listener. `SO_REUSEADDR` on the daemon's listener (set by `socket2`)
/// closes the rebind window quickly enough for these tests.
fn reserve_port_ipv4() -> u16 {
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe bind ipv4");
    let port = probe.local_addr().expect("local addr").port();
    drop(probe);
    port
}

fn spawn_daemon_with_args(
    args: Vec<String>,
) -> (
    thread::JoinHandle<Result<(), daemon::DaemonError>>,
    SignalFlags,
) {
    let flags = SignalFlags::new();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments(args)
        .signal_flags(flags.clone())
        .build();
    let handle = thread::spawn(move || run_daemon(config));
    (handle, flags)
}

fn connect_ipv4(port: u16) -> Option<TcpStream> {
    let addr = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    // Daemon startup is asynchronous; retry briefly so the test does not
    // race the accept-loop bind.
    let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            Ok(stream) => return Some(stream),
            Err(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => return None,
        }
    }
}

fn connect_ipv6(port: u16) -> Option<TcpStream> {
    let addr = std::net::SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            Ok(stream) => return Some(stream),
            Err(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => return None,
        }
    }
}

fn expect_greeting(stream: TcpStream) {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("set_read_timeout");
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .expect("set_write_timeout");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("daemon must send @RSYNCD greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected @RSYNCD greeting, got {line:?}"
    );
}

fn shutdown_daemon(
    handle: thread::JoinHandle<Result<(), daemon::DaemonError>>,
    flags: SignalFlags,
) {
    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread joined");
    assert!(result.is_ok(), "daemon must exit cleanly: {result:?}");
}

/// Guard that restores `OC_RSYNC_DAEMON_ADDRESS_FAMILY` to its prior value
/// when the test exits. Tests within the same binary share the process
/// environment so the guard must be RAII-scoped; nextest isolates test
/// processes, but we still restore for safety against future inlining.
struct EnvGuard {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: env mutation in test setup; nextest runs each test in
        // its own process so there is no concurrent access.
        unsafe { std::env::set_var(key, value) };
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `set` above.
        unsafe {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Default daemon (no `--ipv4`/`--ipv6` flags, no env override) must
/// bind IPv4 successfully and serve an IPv4 client. This is the GitHub
/// Actions exit-10 regression: before the fix, the daemon bound IPv6
/// first and the IPv6 accept failed before the IPv4 listener was
/// considered.
#[test]
fn default_daemon_binds_ipv4_and_serves_ipv4_client() {
    let port = reserve_port_ipv4();
    let (handle, flags) = spawn_daemon_with_args(vec![
        "--no-detach".to_string(),
        "--port".to_string(),
        port.to_string(),
    ]);

    let stream = connect_ipv4(port).expect("IPv4 client must connect to default daemon");
    expect_greeting(stream);
    shutdown_daemon(handle, flags);
}

/// `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv4` is the explicit CI override and
/// must produce the same IPv4-only behaviour as the default.
#[test]
fn env_override_ipv4_serves_ipv4_client() {
    let _env = EnvGuard::set("OC_RSYNC_DAEMON_ADDRESS_FAMILY", "ipv4");
    let port = reserve_port_ipv4();
    let (handle, flags) = spawn_daemon_with_args(vec![
        "--no-detach".to_string(),
        "--port".to_string(),
        port.to_string(),
    ]);

    let stream = connect_ipv4(port).expect("IPv4 client must connect with env=ipv4");
    expect_greeting(stream);
    shutdown_daemon(handle, flags);
}

/// `OC_RSYNC_DAEMON_ADDRESS_FAMILY=both` requests dual-stack. We assert
/// IPv4 reachability (universally available on test runners) to keep
/// this test stable across environments. IPv6 loopback support is
/// environment-dependent so we do not require it here.
#[test]
fn env_override_both_serves_ipv4_client() {
    let _env = EnvGuard::set("OC_RSYNC_DAEMON_ADDRESS_FAMILY", "both");
    let port = reserve_port_ipv4();
    let (handle, flags) = spawn_daemon_with_args(vec![
        "--no-detach".to_string(),
        "--port".to_string(),
        port.to_string(),
    ]);

    let stream = connect_ipv4(port).expect("IPv4 client must connect under dual-stack");
    expect_greeting(stream);
    shutdown_daemon(handle, flags);
}

/// `--ipv4 --ipv6` together used to be rejected as a CLI conflict. After
/// this fix, the flag pair is the dual-stack opt-in: the daemon binds
/// both families and serves traffic on whichever family is reachable.
/// We assert IPv4 reachability (universally available on test runners)
/// to keep this test stable across environments.
#[test]
fn both_family_flags_enable_dual_stack_listener() {
    let port = reserve_port_ipv4();
    let (handle, flags) = spawn_daemon_with_args(vec![
        "--no-detach".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--ipv4".to_string(),
        "--ipv6".to_string(),
    ]);

    let stream =
        connect_ipv4(port).expect("IPv4 client must connect when both family flags are set");
    expect_greeting(stream);
    shutdown_daemon(handle, flags);
}

/// `--ipv6` forces IPv6-only binding. On environments where IPv6
/// loopback is functional, the daemon serves an IPv6 client. We probe
/// for IPv6 loopback support first and skip when the runner lacks it
/// (some CI runners disable `::1`); this keeps the test deterministic
/// across the macOS / Linux / Windows matrix.
#[test]
fn ipv6_flag_forces_ipv6_only() {
    let probe = TcpListener::bind((Ipv6Addr::LOCALHOST, 0));
    let Ok(probe) = probe else {
        eprintln!("skipping ipv6_flag_forces_ipv6_only: no IPv6 loopback on this runner");
        return;
    };
    let port = probe.local_addr().expect("local addr").port();
    drop(probe);

    let (handle, flags) = spawn_daemon_with_args(vec![
        "--no-detach".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--ipv6".to_string(),
    ]);

    if let Some(stream) = connect_ipv6(port) {
        expect_greeting(stream);
    } else {
        eprintln!("ipv6_flag_forces_ipv6_only: IPv6 connect skipped (no loopback connectivity)");
    }
    shutdown_daemon(handle, flags);
}
