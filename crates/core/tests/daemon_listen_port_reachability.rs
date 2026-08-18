//! Readiness must mean "reachable the way the client will dial", not merely
//! "listening on that port".
//!
//! `spawn_daemon_on_free_port` allocates a candidate port, spawns the daemon on
//! it, and polls `daemon_listen_port(pid)` until it reports that port. Every
//! daemon fixture then connects to `rsync://127.0.0.1:<port>/`.
//!
//! On its default bind the daemon opens one socket per address family and,
//! mirroring upstream `socket.c:463-465`, treats a per-family `bind` failure as
//! a warning and serves on whichever families succeeded. Under parallel test
//! load a concurrently-allocated candidate can take the IPv4 half of the port
//! first, leaving the daemon alive and listening on IPv6 only - on the right
//! port, yet refusing the IPv4 connection the test is about to make. A probe
//! that reports any listening port therefore declares readiness for a daemon
//! the client cannot reach, and the connect fails with ECONNREFUSED *after* the
//! readiness wait succeeded - so the bounded retry that exists for exactly this
//! race never fires.
//!
//! These tests pin the discriminating property directly, without a daemon: the
//! probe reports a port only when an IPv4 loopback client could reach it. They
//! rely on nextest's process-per-test isolation, so the only listening sockets
//! in the process are the ones each test creates.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

use test_support::daemon_listen_port;

/// The bug this file exists for. An IPv6-only listener holds the port but
/// cannot serve `127.0.0.1`: the daemon sets `IPV6_V6ONLY` on every IPv6 socket
/// it opens, so not even an IPv4-mapped connection reaches it. Reporting it
/// would let `spawn_daemon_on_free_port` accept a daemon whose next client
/// connection is refused.
#[test]
fn an_ipv6_only_listener_is_not_reported() {
    let Ok(listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
        eprintln!("skipping: no IPv6 loopback on this host");
        return;
    };
    let port = listener.local_addr().expect("local_addr").port();

    assert_ne!(
        daemon_listen_port(std::process::id()),
        Some(port),
        "an IPv6-only listener must not satisfy readiness: the client dials \
         127.0.0.1 and would be refused",
    );
}

/// Non-vacuity for the case above, and the shape that matters in practice: the
/// daemon's default bind is the IPv4 **wildcard**, which `lsof` renders as
/// `*:<port>` - the same text it prints for the IPv6 wildcard. If this stopped
/// being reported, every daemon fixture would time out instead.
#[test]
fn a_wildcard_ipv4_listener_is_reported() {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind IPv4 wildcard");
    let port = listener.local_addr().expect("local_addr").port();

    assert_eq!(
        daemon_listen_port(std::process::id()),
        Some(port),
        "a wildcard IPv4 listener serves 127.0.0.1 and must satisfy readiness",
    );
}

/// The other reachable form: a daemon started with `--address=127.0.0.1` binds
/// loopback explicitly rather than the wildcard.
#[test]
fn a_loopback_ipv4_listener_is_reported() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind IPv4 loopback");
    let port = listener.local_addr().expect("local_addr").port();

    assert_eq!(
        daemon_listen_port(std::process::id()),
        Some(port),
        "a loopback IPv4 listener is exactly what the client dials",
    );
}
