//! Integration tests for `--debug=BIND` producer emissions exposed by the
//! `protocol::bind` helper module.
//!
//! The trace helpers wrap upstream rsync 3.4.1's per-address-family
//! `socket(2)` and `bind(2)` failure messages from `socket.c:432-470`,
//! flushed through the `BIND` debug gate at `socket.c:479-486`. These
//! tests drive each helper through the real [`logging`] channel - the
//! same path users hit when running `oc-rsyncd --debug=BIND`.

use std::io;

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use protocol::bind::{trace_bind_failure, trace_socket_failure};

/// Initialises logging with the supplied BIND debug level and drains any
/// pending events so the per-test assertions can focus on emissions
/// produced by the test body itself.
fn init_bind(level: u8) {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.bind = level;
    init(cfg);
    let _ = drain_events();
}

/// Collects BIND-flagged debug messages emitted since the last drain.
fn bind_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Bind,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Builds a synthetic `io::Error` with a stable message so the assertions
/// can pin both the prefix and the trailing address-family suffix without
/// depending on the host platform's `strerror(EADDRINUSE)` text.
fn fake_error(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, msg.to_string())
}

/// Pins the level 1 `bind() failed:` shape against upstream
/// `socket.c:463-465`.
#[test]
fn bind_failure_matches_upstream_format_for_ipv6() {
    init_bind(1);
    let err = fake_error("Address already in use");
    trace_bind_failure(10, &err);
    let msgs = bind_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "bind() failed: Address already in use (address-family 10)"),
        "expected upstream-format BIND,1 bind-failure line, got {msgs:?}"
    );
}

/// Pins the level 1 `bind() failed:` shape for the IPv4 address-family
/// integer (`AF_INET = 2`).
#[test]
fn bind_failure_matches_upstream_format_for_ipv4() {
    init_bind(1);
    let err = fake_error("Permission denied");
    trace_bind_failure(2, &err);
    let msgs = bind_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "bind() failed: Permission denied (address-family 2)"),
        "expected upstream-format BIND,1 bind-failure line for IPv4, got {msgs:?}"
    );
}

/// Pins the level 1 `socket() failed:` shape against upstream
/// `socket.c:433-436`.
#[test]
fn socket_failure_matches_upstream_format() {
    init_bind(1);
    let err = fake_error("Address family not supported");
    trace_socket_failure(10, 1, 6, &err);
    let msgs = bind_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "socket(10,1,6) failed: Address family not supported"),
        "expected upstream-format BIND,1 socket-failure line, got {msgs:?}"
    );
}

/// Level 0 suppresses every BIND emission, mirroring upstream's
/// `DEBUG_GTE(BIND, _)` gate.
#[test]
fn all_helpers_silent_under_debug_bind_level_0() {
    init_bind(0);
    let err = fake_error("Address already in use");
    trace_bind_failure(2, &err);
    trace_socket_failure(2, 1, 6, &err);
    assert!(
        bind_messages().is_empty(),
        "all BIND emissions must be silent at level 0"
    );
}

/// Multiple invocations of `trace_bind_failure` produce one emission per
/// call, matching upstream's per-address-family accumulation.
#[test]
fn bind_failure_emits_one_line_per_call() {
    init_bind(1);
    let err = fake_error("Address already in use");
    trace_bind_failure(2, &err);
    trace_bind_failure(10, &err);
    let msgs = bind_messages();
    let ipv4 = msgs
        .iter()
        .filter(|m| m.ends_with("(address-family 2)"))
        .count();
    let ipv6 = msgs
        .iter()
        .filter(|m| m.ends_with("(address-family 10)"))
        .count();
    assert_eq!(ipv4, 1, "expected one IPv4 bind-failure emission");
    assert_eq!(ipv6, 1, "expected one IPv6 bind-failure emission");
}
