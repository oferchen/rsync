//! Regression tests for the daemon listener address-family selection.
//!
//! The default daemon listener binds IPv6 then IPv4 when no explicit family
//! override is given, matching upstream rsync's `default_af_hint = 0`
//! (AF_UNSPEC) behaviour. `bind_listeners_per_family` warns on each
//! per-family failure and only fails the daemon when zero sockets bound,
//! so GitHub Actions Linux runners with a partially configured IPv6 stack
//! (where `bind(2)` to `[::]:port` returns `EADDRNOTAVAIL`) cleanly degrade
//! to the IPv4 listener instead of producing an opaque
//! `error in socket I/O (code 10)` daemon exit. These tests drive
//! `run_daemon` with various `--ipv4` / `--ipv6` and
//! `OC_RSYNC_DAEMON_ADDRESS_FAMILY` configurations and verify a TCP client
//! on the expected family receives the `@RSYNCD:` greeting.
//!
//! Each daemon binds an ephemeral port through the bound-port test seam and
//! reports it before it accepts, so the test connects once, to a port the
//! daemon already listens on.
//!
//! upstream: socket.c:560-658 (`open_socket_in`) iterates every
//! getaddrinfo result and only fails when zero sockets bound.

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};

#[cfg(not(unix))]
use daemon::DaemonConfigBuilder;
use daemon::{DaemonConfig, DaemonError, run_daemon};

// The forked test-daemon harness the unit tests use; each binary uses a subset.
#[allow(dead_code)]
#[path = "../src/tests/daemon_process.rs"]
mod daemon_process;
use daemon_process::DaemonHandle;

/// Starts a daemon with `args` and returns it with the port it listens on.
///
/// The daemon reports the port once every family it could bind is listening,
/// so a client connects without retrying. A daemon that closes the pipe
/// without a port fails the test with how it ended.
fn spawn_daemon(args: &[&str]) -> (DaemonHandle, u16) {
    let (reader, writer) = std::io::pipe().expect("bound-port pipe");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments(args.iter().copied())
        .report_bound_port(writer)
        .build();
    let handle = DaemonHandle::spawn(config);
    let mut line = String::new();
    BufReader::new(reader)
        .read_line(&mut line)
        .expect("read the bound port");
    match line.trim().parse() {
        Ok(port) => (handle, port),
        Err(_) => {
            // No port means no listener; stop a daemon still running so the
            // join reports how it ended instead of waiting on it.
            handle.terminate();
            panic!("daemon reported no port {line:?}: {:?}", handle.join())
        }
    }
}

fn expect_greeting(stream: TcpStream) {
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

fn shutdown_daemon(handle: DaemonHandle) {
    handle.terminate();
    let result = handle.join().expect("daemon joined");
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
/// serve an IPv4 client. With dual-stack default the IPv6 bind is
/// attempted first; the helper logs a warning and falls back to the
/// IPv4 listener when IPv6 is unavailable on the runner, so the IPv4
/// client receives `@RSYNCD:` either way. This is the regression for
/// the GitHub Actions exit-10 failure where the silent IPv6 swallow
/// previously masked the IPv4 listener startup.
#[test]
fn default_daemon_serves_ipv4_client_via_dual_stack() {
    let (handle, port) = spawn_daemon(&["--no-detach"]);

    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .expect("IPv4 client must connect to default daemon");
    expect_greeting(stream);
    shutdown_daemon(handle);
}

/// `OC_RSYNC_DAEMON_ADDRESS_FAMILY=ipv4` is the explicit CI override and
/// forces an IPv4-only listener that must still serve an IPv4 client.
#[test]
fn env_override_ipv4_serves_ipv4_client() {
    let _env = EnvGuard::set("OC_RSYNC_DAEMON_ADDRESS_FAMILY", "ipv4");
    let (handle, port) = spawn_daemon(&["--no-detach"]);

    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .expect("IPv4 client must connect with env=ipv4");
    expect_greeting(stream);
    shutdown_daemon(handle);
}

/// `OC_RSYNC_DAEMON_ADDRESS_FAMILY=both` requests dual-stack. We assert
/// IPv4 reachability (universally available on test runners) to keep
/// this test stable across environments. IPv6 loopback support is
/// environment-dependent so we do not require it here.
#[test]
fn env_override_both_serves_ipv4_client() {
    let _env = EnvGuard::set("OC_RSYNC_DAEMON_ADDRESS_FAMILY", "both");
    let (handle, port) = spawn_daemon(&["--no-detach"]);

    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .expect("IPv4 client must connect under dual-stack");
    expect_greeting(stream);
    shutdown_daemon(handle);
}

/// `--ipv4 --ipv6` together is the dual-stack opt-in: the daemon binds
/// both families and serves traffic on whichever family is reachable.
/// We assert IPv4 reachability (universally available on test runners)
/// to keep this test stable across environments.
#[test]
fn both_family_flags_enable_dual_stack_listener() {
    let (handle, port) = spawn_daemon(&["--no-detach", "--ipv4", "--ipv6"]);

    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .expect("IPv4 client must connect when both family flags are set");
    expect_greeting(stream);
    shutdown_daemon(handle);
}

/// `--ipv6` forces IPv6-only binding. On environments where IPv6
/// loopback is functional, the daemon serves an IPv6 client. We probe
/// for IPv6 loopback support first and skip when the runner lacks it
/// (some CI runners disable `::1`).
#[test]
fn ipv6_flag_forces_ipv6_only() {
    if TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_err() {
        eprintln!("skipping ipv6_flag_forces_ipv6_only: no IPv6 loopback on this runner");
        return;
    }
    let (handle, port) = spawn_daemon(&["--no-detach", "--ipv6"]);

    let stream = TcpStream::connect((Ipv6Addr::LOCALHOST, port))
        .expect("IPv6 client must connect to an --ipv6 daemon");
    expect_greeting(stream);
    shutdown_daemon(handle);
}
