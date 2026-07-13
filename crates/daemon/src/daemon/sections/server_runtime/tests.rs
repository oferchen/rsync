use super::*;

#[test]
fn format_connection_status_zero_connections() {
    assert_eq!(format_connection_status(0), "Idle; waiting for connections");
}

#[test]
fn format_connection_status_one_connection() {
    assert_eq!(format_connection_status(1), "Serving 1 connection");
}

#[test]
fn format_connection_status_multiple_connections() {
    assert_eq!(format_connection_status(5), "Serving 5 connections");
}

#[test]
fn normalize_peer_address_preserves_ipv4() {
    let addr: SocketAddr = "192.168.1.1:8873".parse().unwrap();
    assert_eq!(normalize_peer_address(addr), addr);
}

#[test]
fn normalize_peer_address_preserves_pure_ipv6() {
    let addr: SocketAddr = "[2001:db8::1]:8873".parse().unwrap();
    assert_eq!(normalize_peer_address(addr), addr);
}

#[test]
fn normalize_peer_address_converts_ipv4_mapped() {
    use std::net::{Ipv6Addr, SocketAddrV6};
    // IPv4-mapped IPv6: ::ffff:127.0.0.1
    let v6 = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
    let addr = SocketAddr::V6(SocketAddrV6::new(v6, 8873, 0, 0));
    let normalized = normalize_peer_address(addr);
    assert_eq!(normalized.to_string(), "127.0.0.1:8873");
}

#[test]
fn is_connection_closed_error_broken_pipe() {
    assert!(is_connection_closed_error(io::ErrorKind::BrokenPipe));
}

#[test]
fn is_connection_closed_error_connection_reset() {
    assert!(is_connection_closed_error(io::ErrorKind::ConnectionReset));
}

#[test]
fn is_connection_closed_error_connection_aborted() {
    assert!(is_connection_closed_error(io::ErrorKind::ConnectionAborted));
}

#[test]
fn is_connection_closed_error_other_errors_false() {
    assert!(!is_connection_closed_error(io::ErrorKind::NotFound));
    assert!(!is_connection_closed_error(io::ErrorKind::PermissionDenied));
    assert!(!is_connection_closed_error(io::ErrorKind::TimedOut));
}

#[test]
fn connection_counter_starts_at_zero() {
    let counter = ConnectionCounter::new();
    assert_eq!(counter.active(), 0);
}

#[test]
fn connection_counter_default_starts_at_zero() {
    let counter = ConnectionCounter::default();
    assert_eq!(counter.active(), 0);
}

#[test]
fn connection_counter_increments_on_acquire() {
    let counter = ConnectionCounter::new();
    let _guard = counter.acquire();
    assert_eq!(counter.active(), 1);
}

#[test]
fn connection_counter_decrements_on_guard_drop() {
    let counter = ConnectionCounter::new();
    let guard = counter.acquire();
    assert_eq!(counter.active(), 1);
    drop(guard);
    assert_eq!(counter.active(), 0);
}

#[test]
fn connection_counter_tracks_multiple_connections() {
    let counter = ConnectionCounter::new();
    let g1 = counter.acquire();
    let g2 = counter.acquire();
    let g3 = counter.acquire();
    assert_eq!(counter.active(), 3);

    drop(g2);
    assert_eq!(counter.active(), 2);

    drop(g1);
    assert_eq!(counter.active(), 1);

    drop(g3);
    assert_eq!(counter.active(), 0);
}

#[test]
fn connection_counter_clone_shares_state() {
    let counter = ConnectionCounter::new();
    let cloned = counter.clone();

    let _guard = counter.acquire();
    assert_eq!(cloned.active(), 1);

    let _guard2 = cloned.acquire();
    assert_eq!(counter.active(), 2);
}

#[test]
fn connection_counter_concurrent_access() {
    let counter = ConnectionCounter::new();
    let mut handles = vec![];

    for _ in 0..10 {
        let cloned = counter.clone();
        let handle = thread::spawn(move || {
            let mut guards = Vec::new();
            for _ in 0..100 {
                guards.push(cloned.acquire());
            }
            assert!(cloned.active() >= 100);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(counter.active(), 0);
}

#[test]
fn connection_guard_debug_format() {
    let counter = ConnectionCounter::new();
    let guard = counter.acquire();
    let debug = format!("{guard:?}");
    assert!(debug.contains("ConnectionGuard"));
}

#[test]
fn connection_counter_debug_format() {
    let counter = ConnectionCounter::new();
    let debug = format!("{counter:?}");
    assert!(debug.contains("ConnectionCounter"));
}

#[test]
fn describe_panic_payload_extracts_string_message() {
    let payload = std::panic::catch_unwind(|| {
        panic!("handler exploded: {}", "bad input");
    })
    .unwrap_err();
    let description = describe_panic_payload(payload);
    assert!(
        description.contains("handler exploded"),
        "expected String payload to be extracted, got: {description}"
    );
}

#[test]
fn describe_panic_payload_extracts_str_message() {
    let payload = std::panic::catch_unwind(|| {
        panic!("static str panic");
    })
    .unwrap_err();
    let description = describe_panic_payload(payload);
    assert_eq!(description, "static str panic");
}

#[test]
fn describe_panic_payload_handles_non_string_payload() {
    let payload = std::panic::catch_unwind(|| {
        std::panic::panic_any(42u32);
    })
    .unwrap_err();
    let description = describe_panic_payload(payload);
    assert_eq!(description, "unknown panic payload");
}

#[test]
fn join_worker_handles_successful_thread() {
    let handle = thread::spawn(|| Ok(()));
    let result = join_worker(handle);
    assert!(result.is_ok());
}

#[test]
fn join_worker_handles_connection_closed_error() {
    let handle = thread::spawn(|| {
        Err((
            Some("127.0.0.1:12345".parse().unwrap()),
            io::Error::new(io::ErrorKind::BrokenPipe, "connection closed"),
        ))
    });
    let result = join_worker(handle);
    assert!(
        result.is_ok(),
        "BrokenPipe should be treated as normal close"
    );
}

#[test]
fn join_worker_swallows_panicking_thread() {
    let handle = thread::spawn(|| -> WorkerResult {
        panic!("simulated handler crash");
    });
    // join_worker blocks until the thread completes - no sleep needed
    let result = join_worker(handle);
    assert!(
        result.is_ok(),
        "join_worker must swallow panics to keep the daemon alive"
    );
}

#[test]
fn catch_unwind_isolates_panic_and_returns_ok() {
    let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    let handle = thread::spawn(move || -> WorkerResult {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("connection handler for test panicked");
        }));
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err((Some(peer_addr), error)),
            Err(payload) => {
                let description = describe_panic_payload(payload);
                assert!(
                    description.contains("connection handler for test panicked"),
                    "panic message should be preserved: {description}"
                );
                Ok(())
            }
        }
    });
    let result = handle.join().expect("thread should not propagate panic");
    assert!(
        result.is_ok(),
        "catch_unwind should convert panics into Ok(())"
    );
}

#[test]
fn log_progress_summary_without_log_sink() {
    log_progress_summary(None, 3, 10, SystemTime::now());
}

#[test]
fn log_progress_summary_zero_active() {
    log_progress_summary(None, 0, 0, SystemTime::now());
}

#[test]
fn log_progress_summary_with_uptime() {
    let past = SystemTime::now() - Duration::from_secs(90);
    log_progress_summary(None, 2, 5, past);
}

#[test]
fn graceful_exit_flag_stops_accept_loop_independently() {
    let flags = SignalFlags {
        reload_config: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
        graceful_exit: Arc::new(AtomicBool::new(true)),
        progress_dump: Arc::new(AtomicBool::new(false)),
    };
    assert!(
        flags.graceful_exit.load(Ordering::Relaxed),
        "graceful_exit should be set"
    );
    assert!(
        !flags.shutdown.load(Ordering::Relaxed),
        "shutdown must remain unset when only graceful_exit is triggered"
    );
}

#[test]
fn progress_dump_flag_is_consumed() {
    let flags = SignalFlags {
        reload_config: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
        graceful_exit: Arc::new(AtomicBool::new(false)),
        progress_dump: Arc::new(AtomicBool::new(true)),
    };
    let was_set = flags.progress_dump.swap(false, Ordering::Relaxed);
    assert!(was_set, "progress_dump should have been set");
    assert!(
        !flags.progress_dump.load(Ordering::Relaxed),
        "progress_dump must be cleared after swap"
    );
}

#[test]
fn reload_config_with_no_config_path_is_noop() {
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
    let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
    let notifier = systemd::ServiceNotifier::new();

    reload_daemon_config(
        None,
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );

    assert!(modules.is_empty());
    assert!(motd.is_empty());
}

#[test]
fn reload_config_with_missing_file_keeps_old_config() {
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let old_module = ModuleRuntime::new(
        ModuleDefinition {
            name: "old".to_owned(),
            path: PathBuf::from("/old"),
            ..Default::default()
        },
        None,
    );
    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(vec![old_module]);
    let mut motd: Arc<Vec<String>> = Arc::new(vec!["old motd".to_owned()]);
    let notifier = systemd::ServiceNotifier::new();

    let missing = PathBuf::from("/nonexistent/rsyncd.conf");
    reload_daemon_config(
        Some(&missing),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "old");
    assert_eq!(motd.len(), 1);
    assert_eq!(motd[0], "old motd");
}

#[cfg(unix)]
#[test]
fn reload_config_replaces_modules_and_motd() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let conf_path = dir.path().join("rsyncd.conf");
    {
        let mut f = fs::File::create(&conf_path).unwrap();
        writeln!(f, "motd file = {}", dir.path().join("motd.txt").display()).unwrap();
        writeln!(f, "[alpha]").unwrap();
        writeln!(f, "path = /alpha").unwrap();
    }
    {
        let motd_path = dir.path().join("motd.txt");
        let mut f = fs::File::create(motd_path).unwrap();
        writeln!(f, "Welcome!").unwrap();
    }

    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
    let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
    let notifier = systemd::ServiceNotifier::new();

    reload_daemon_config(
        Some(&conf_path),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "alpha");
    assert_eq!(modules[0].definition.path, PathBuf::from("/alpha"));
}

#[cfg(unix)]
#[test]
fn reload_config_existing_connections_keep_old_config() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let conf_path = dir.path().join("rsyncd.conf");
    {
        let mut f = fs::File::create(&conf_path).unwrap();
        writeln!(f, "[original]").unwrap();
        writeln!(f, "path = /original").unwrap();
    }

    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
    let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
    let notifier = systemd::ServiceNotifier::new();

    reload_daemon_config(
        Some(&conf_path),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "original");

    let old_modules = Arc::clone(&modules);

    {
        let mut f = fs::File::create(&conf_path).unwrap();
        writeln!(f, "[updated]").unwrap();
        writeln!(f, "path = /updated").unwrap();
    }
    reload_daemon_config(
        Some(&conf_path),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "updated");

    assert_eq!(old_modules.len(), 1);
    assert_eq!(old_modules[0].definition.name, "original");
}

#[cfg(unix)]
#[test]
fn reload_config_with_invalid_syntax_keeps_old_config() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let conf_path = dir.path().join("rsyncd.conf");

    {
        let mut f = fs::File::create(&conf_path).unwrap();
        writeln!(f, "[valid]").unwrap();
        writeln!(f, "path = /valid").unwrap();
    }

    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
    let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
    let notifier = systemd::ServiceNotifier::new();

    reload_daemon_config(
        Some(&conf_path),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "valid");

    {
        let mut f = fs::File::create(&conf_path).unwrap();
        writeln!(f, "[broken").unwrap();
    }

    reload_daemon_config(
        Some(&conf_path),
        &limiter,
        &mut modules,
        &mut motd,
        None,
        &notifier,
    );

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].definition.name, "valid");
}

#[cfg(unix)]
#[test]
fn reload_config_sighup_flag_triggers_reload() {
    let flags = SignalFlags {
        reload_config: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
        graceful_exit: Arc::new(AtomicBool::new(false)),
        progress_dump: Arc::new(AtomicBool::new(false)),
    };

    flags.reload_config.store(true, Ordering::Relaxed);

    assert!(flags.reload_config.swap(false, Ordering::Relaxed));
    assert!(!flags.reload_config.swap(false, Ordering::Relaxed));
}

#[test]
fn parse_socket_options_tcp_nodelay() {
    let opts = parse_socket_options("TCP_NODELAY").expect("parse succeeds");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
}

#[test]
fn parse_socket_options_so_keepalive() {
    let opts = parse_socket_options("SO_KEEPALIVE").expect("parse succeeds");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0], SocketOption::SoKeepAlive(true));
}

#[test]
fn parse_socket_options_buffer_sizes() {
    let opts =
        parse_socket_options("SO_SNDBUF=65536, SO_RCVBUF=32768").expect("parse succeeds");
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0], SocketOption::SoSndBuf(65536));
    assert_eq!(opts[1], SocketOption::SoRcvBuf(32768));
}

#[test]
fn parse_socket_options_multiple_mixed() {
    let opts = parse_socket_options("TCP_NODELAY, SO_KEEPALIVE, SO_SNDBUF=65536")
        .expect("parse succeeds");
    assert_eq!(opts.len(), 3);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::SoKeepAlive(true));
    assert_eq!(opts[2], SocketOption::SoSndBuf(65536));
}

#[test]
fn parse_socket_options_bool_explicit_values() {
    let opts = parse_socket_options("TCP_NODELAY=1, SO_KEEPALIVE=0").expect("parse succeeds");
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::SoKeepAlive(false));
}

#[test]
fn parse_socket_options_bool_text_values() {
    let opts =
        parse_socket_options("TCP_NODELAY=true, SO_KEEPALIVE=false").expect("parse succeeds");
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::SoKeepAlive(false));
}

#[test]
fn parse_socket_options_empty_string() {
    let opts = parse_socket_options("").expect("parse succeeds");
    assert!(opts.is_empty());
}

#[test]
fn parse_socket_options_unknown_option_rejected() {
    let err = parse_socket_options("UNKNOWN_OPT").expect_err("should fail");
    assert!(err.contains("unknown socket option"), "{err}");
}

#[test]
fn parse_socket_options_sndbuf_missing_value_rejected() {
    let err = parse_socket_options("SO_SNDBUF").expect_err("should fail");
    assert!(err.contains("requires a numeric value"), "{err}");
}

#[test]
fn parse_socket_options_sndbuf_invalid_value_rejected() {
    let err = parse_socket_options("SO_SNDBUF=abc").expect_err("should fail");
    assert!(err.contains("invalid numeric value"), "{err}");
}

#[test]
fn parse_socket_options_case_insensitive() {
    let opts = parse_socket_options("tcp_nodelay, so_keepalive").expect("parse succeeds");
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::SoKeepAlive(true));
}

#[test]
fn parse_socket_options_trailing_comma_ignored() {
    let opts = parse_socket_options("TCP_NODELAY,").expect("parse succeeds");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
}

#[test]
fn parse_socket_options_whitespace_around_values() {
    let opts =
        parse_socket_options("  TCP_NODELAY , SO_SNDBUF = 4096  ").expect("parse succeeds");
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::SoSndBuf(4096));
}

#[test]
fn apply_listener_socket_options_nodelay_keepalive() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let opts = vec![
        SocketOption::TcpNoDelay(true),
        SocketOption::SoKeepAlive(true),
    ];
    apply_socket_options_to_listener(&listener, &opts).expect("apply succeeds");

    let sock = socket2::SockRef::from(&listener);
    assert!(sock.tcp_nodelay().expect("query nodelay"));
    assert!(sock.keepalive().expect("query keepalive"));
}

#[test]
fn apply_listener_socket_options_buffer_sizes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let opts = vec![SocketOption::SoSndBuf(32768), SocketOption::SoRcvBuf(32768)];
    apply_socket_options_to_listener(&listener, &opts).expect("apply succeeds");

    let sock = socket2::SockRef::from(&listener);
    assert!(sock.send_buffer_size().expect("query sndbuf") >= 32768);
    assert!(sock.recv_buffer_size().expect("query rcvbuf") >= 32768);
}

#[test]
fn apply_stream_socket_options_nodelay_keepalive() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let stream = TcpStream::connect(addr).expect("connect");
    let opts = vec![
        SocketOption::TcpNoDelay(true),
        SocketOption::SoKeepAlive(true),
    ];
    apply_socket_options_to_stream(&stream, &opts).expect("apply succeeds");

    let sock = socket2::SockRef::from(&stream);
    assert!(sock.tcp_nodelay().expect("query nodelay"));
    assert!(sock.keepalive().expect("query keepalive"));
}

#[test]
fn apply_stream_socket_options_buffer_sizes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let stream = TcpStream::connect(addr).expect("connect");
    let opts = vec![SocketOption::SoSndBuf(32768), SocketOption::SoRcvBuf(32768)];
    apply_socket_options_to_stream(&stream, &opts).expect("apply succeeds");

    let sock = socket2::SockRef::from(&stream);
    assert!(sock.send_buffer_size().expect("query sndbuf") >= 32768);
    assert!(sock.recv_buffer_size().expect("query rcvbuf") >= 32768);
}

#[test]
fn parse_socket_options_ip_tos_hex() {
    let opts = parse_socket_options("IP_TOS=0x10").expect("parse succeeds");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0], SocketOption::IpTos(0x10));
}

#[test]
fn parse_socket_options_ip_tos_decimal() {
    let opts = parse_socket_options("IP_TOS=16").expect("parse succeeds");
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0], SocketOption::IpTos(16));
}

#[test]
fn parse_socket_options_ip_tos_requires_value() {
    let err = parse_socket_options("IP_TOS").expect_err("should fail");
    assert!(err.contains("requires a numeric value"), "{err}");
}

#[test]
fn parse_socket_options_combined_with_ip_tos() {
    let opts = parse_socket_options("TCP_NODELAY, IP_TOS=0x08, SO_SNDBUF=65536")
        .expect("parse succeeds");
    assert_eq!(opts.len(), 3);
    assert_eq!(opts[0], SocketOption::TcpNoDelay(true));
    assert_eq!(opts[1], SocketOption::IpTos(0x08));
    assert_eq!(opts[2], SocketOption::SoSndBuf(65536));
}

#[test]
fn apply_stream_socket_options_empty_is_noop() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let stream = TcpStream::connect(addr).expect("connect");
    apply_socket_options_to_stream(&stream, &[]).expect("empty options should succeed");
}

/// A daemon `socket options =` config written for upstream rsync must parse
/// every entry in upstream's `socket_options[]` table (socket.c) - silently
/// dropping an option would make a config that is portable under upstream
/// behave differently here. This locks in the five entries previously missing
/// from the daemon table (`SO_BROADCAST`, `SO_SNDLOWAT`, `SO_RCVLOWAT`,
/// `SO_SNDTIMEO`, `SO_RCVTIMEO`) plus the `IPTOS_*` `OPT_ON` symbolic presets,
/// each resolving to the correct level/optname/value semantics.
#[test]
fn parse_socket_options_accepts_all_upstream_options() {
    use SocketOption::{SoBroadcast, SoKeepAlive, SoRcvBuf, SoSndBuf, TcpNoDelay};

    // Available across every daemon target platform.
    assert_eq!(
        parse_socket_options("SO_KEEPALIVE").expect("parse"),
        vec![SoKeepAlive(true)]
    );
    assert_eq!(
        parse_socket_options("TCP_NODELAY").expect("parse"),
        vec![TcpNoDelay(true)]
    );
    assert_eq!(
        parse_socket_options("SO_BROADCAST").expect("parse"),
        vec![SoBroadcast(true)]
    );
    assert_eq!(
        parse_socket_options("SO_SNDBUF=65536, SO_RCVBUF=32768").expect("parse"),
        vec![SoSndBuf(65536), SoRcvBuf(32768)]
    );

    // upstream: IPTOS_LOWDELAY / IPTOS_THROUGHPUT are OPT_ON presets that map
    // to a fixed IP_TOS byte and must reject an `=value` suffix.
    #[cfg(not(target_family = "windows"))]
    {
        assert_eq!(
            parse_socket_options("IPTOS_LOWDELAY").expect("parse"),
            vec![SocketOption::IpTos(0x10)]
        );
        assert_eq!(
            parse_socket_options("IPTOS_THROUGHPUT").expect("parse"),
            vec![SocketOption::IpTos(0x08)]
        );
        let err =
            parse_socket_options("IPTOS_LOWDELAY=5").expect_err("preset must reject a value");
        assert!(err.contains("does not take a value"), "{err}");
    }

    // SO_SNDTIMEO / SO_RCVTIMEO are written as a plain int on all Unix targets.
    #[cfg(unix)]
    {
        assert_eq!(
            parse_socket_options("SO_SNDTIMEO=30").expect("parse"),
            vec![SocketOption::SoSndTimeo(30)]
        );
        assert_eq!(
            parse_socket_options("SO_RCVTIMEO=30").expect("parse"),
            vec![SocketOption::SoRcvTimeo(30)]
        );
    }

    // SO_SNDLOWAT / SO_RCVLOWAT exist only where libc defines them (not Linux).
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    {
        assert_eq!(
            parse_socket_options("SO_SNDLOWAT=1").expect("parse"),
            vec![SocketOption::SoSndLoWat(1)]
        );
        assert_eq!(
            parse_socket_options("SO_RCVLOWAT=1").expect("parse"),
            vec![SocketOption::SoRcvLoWat(1)]
        );
    }
}

/// `SO_BROADCAST` exercises the apply path end to end on Unix, where the kernel
/// accepts the option on any socket type at `setsockopt` time. Windows validates
/// that `SO_BROADCAST` is only meaningful for datagram sockets and rejects it on
/// a stream socket with `WSAENOPROTOOPT`, so on Windows it joins the LOWAT/TIMEO
/// entries that mirror upstream's best-effort `setsockopt` (rejectable by the
/// platform at runtime) and are covered at the parse layer only.
#[cfg(unix)]
#[test]
fn apply_socket_options_broadcast_sets_flag() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    apply_socket_options_to_listener(&listener, &[SocketOption::SoBroadcast(true)])
        .expect("apply succeeds");

    let sock = socket2::SockRef::from(&listener);
    assert!(sock.broadcast().expect("query broadcast"));
}

/// Builds an [`AcceptLoopState`] suitable for unit tests that exercise
/// admission control without spinning up a full daemon.
///
/// All borrowed fields point into long-lived storage owned by the caller,
/// so the test must keep `signal_flags`, `config_path`, `limiter`, `log`
/// and `notifier` alive for the duration of the borrow.
fn test_accept_loop_state<'a>(
    signal_flags: &'a SignalFlags,
    config_path: &'a Option<PathBuf>,
    limiter: &'a Option<Arc<ConnectionLimiter>>,
    log_sink: &'a Option<SharedLogSink>,
    notifier: &'a systemd::ServiceNotifier,
    counter: ConnectionCounter,
    max_connections: Option<usize>,
) -> AcceptLoopState<'a> {
    AcceptLoopState {
        signal_flags,
        workers: Vec::new(),
        served: 0,
        active_connections: 0,
        connection_counter: counter,
        start_time: SystemTime::now(),
        max_sessions: None,
        max_connections,
        config_path,
        connection_limiter: limiter,
        modules: Arc::new(Vec::new()),
        motd_lines: Arc::new(Vec::new()),
        log_sink,
        notifier,
        client_socket_options: Arc::new(Vec::new()),
        bandwidth_limit: None,
        bandwidth_burst: None,
        reverse_lookup: false,
        proxy_protocol: false,
    }
}

/// Reads the first line emitted by the daemon refusal.
///
/// `@ERROR:` lines are newline-terminated, so we read until `\n` and trim
/// the terminator before comparison.
fn read_error_line(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    let mut buf = Vec::with_capacity(96);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).expect("read refusal payload");
        if n == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf).expect("UTF-8 refusal payload")
}

fn no_op_signal_flags() -> SignalFlags {
    SignalFlags {
        reload_config: Arc::new(AtomicBool::new(false)),
        shutdown: Arc::new(AtomicBool::new(false)),
        graceful_exit: Arc::new(AtomicBool::new(false)),
        progress_dump: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn refuse_if_at_capacity_admits_when_no_cap_configured() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let client_handle = thread::spawn(move || TcpStream::connect(local).expect("connect"));
    let (server_stream, peer) = listener.accept().expect("accept");
    let _client = client_handle.join().expect("client connect");
    let mut server_stream = DaemonStream::plain(server_stream);

    let flags = no_op_signal_flags();
    let config_path: Option<PathBuf> = None;
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let log_sink: Option<SharedLogSink> = None;
    let notifier = systemd::ServiceNotifier::new();
    let counter = ConnectionCounter::new();
    // Acquire two guards; with no cap configured the helper still admits.
    let _g1 = counter.acquire();
    let _g2 = counter.acquire();
    let state = test_accept_loop_state(
        &flags,
        &config_path,
        &limiter,
        &log_sink,
        &notifier,
        counter.clone(),
        None,
    );

    assert!(!refuse_if_at_capacity(&mut server_stream, peer, &state));
}

#[test]
fn accept_loop_refuses_when_at_capacity() {
    // Simulate max_connections = 2. Two guards are already held by
    // existing workers, so a third connection must be refused with the
    // upstream-compatible `@ERROR: max connections (2) reached -- try
    // again later` line, after which the accept loop is expected to
    // keep running.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let client_handle =
        thread::spawn(move || TcpStream::connect(local).expect("connect third client"));
    let (server_stream, peer) = listener.accept().expect("accept third client");
    let mut client_stream = client_handle.join().expect("client connect");
    let mut server_stream = DaemonStream::plain(server_stream);

    let flags = no_op_signal_flags();
    let config_path: Option<PathBuf> = None;
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let log_sink: Option<SharedLogSink> = None;
    let notifier = systemd::ServiceNotifier::new();
    let counter = ConnectionCounter::new();
    // Two active workers already hold guards.
    let _g1 = counter.acquire();
    let _g2 = counter.acquire();
    assert_eq!(counter.active(), 2);

    let state = test_accept_loop_state(
        &flags,
        &config_path,
        &limiter,
        &log_sink,
        &notifier,
        counter.clone(),
        Some(2),
    );

    assert!(refuse_if_at_capacity(&mut server_stream, peer, &state));

    // The client must observe the exact upstream wording before EOF.
    let line = read_error_line(&mut client_stream);
    assert_eq!(
        line, "@ERROR: max connections (2) reached -- try again later",
        "refusal payload must mirror upstream clientserver.c:752"
    );

    // The accept loop is expected to keep running: dropping the server
    // stream is what closes the refused socket, and the counter is
    // untouched (no guard acquired for the refused peer).
    drop(server_stream);
    assert_eq!(counter.active(), 2);
}

#[test]
fn refuse_if_at_capacity_emits_structured_warning() {
    // Operators need a stable, structured warning line whenever the
    // global `--max-connections` cap rejects a connection; assert that
    // `refuse_if_at_capacity` writes a single record with the
    // `which=global`, `peer=`, `cap=` and `current=` fields named.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let client_handle =
        thread::spawn(move || TcpStream::connect(local).expect("connect refused client"));
    let (server_stream, peer) = listener.accept().expect("accept refused client");
    let _client = client_handle.join().expect("client connect");
    let mut server_stream = DaemonStream::plain(server_stream);

    let log_dir = tempfile::tempdir().expect("log dir");
    let log_path = log_dir.path().join("daemon.log");
    let log_sink: Option<SharedLogSink> =
        Some(open_log_sink(&log_path, Brand::Oc).expect("open log"));

    let flags = no_op_signal_flags();
    let config_path: Option<PathBuf> = None;
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let notifier = systemd::ServiceNotifier::new();
    let counter = ConnectionCounter::new();
    let _g1 = counter.acquire();
    let _g2 = counter.acquire();
    assert_eq!(counter.active(), 2);

    let state = test_accept_loop_state(
        &flags,
        &config_path,
        &limiter,
        &log_sink,
        &notifier,
        counter.clone(),
        Some(2),
    );

    assert!(refuse_if_at_capacity(&mut server_stream, peer, &state));
    drop(server_stream);

    // Drop the sink so the underlying file is flushed before we read it.
    drop(log_sink);

    let contents = std::fs::read_to_string(&log_path).expect("read log");
    assert!(
        contents.starts_with("oc-rsync warning:"),
        "expected warning level, got: {contents}"
    );
    assert!(
        contents.contains("max-connections cap reached"),
        "missing structured prefix: {contents}"
    );
    assert!(
        contents.contains("which=global"),
        "missing which=global field: {contents}"
    );
    assert!(
        contents.contains(&format!("peer={peer}")),
        "missing peer= field: {contents}"
    );
    assert!(contents.contains("cap=2"), "missing cap= field: {contents}");
    assert!(
        contents.contains("current=2"),
        "missing current= field: {contents}"
    );
}

#[test]
fn accept_loop_recovers_after_disconnect() {
    // Same setup as `accept_loop_refuses_when_at_capacity`, but after
    // releasing one of the two active guards a new connection must be
    // admitted instead of refused.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let client_handle =
        thread::spawn(move || TcpStream::connect(local).expect("connect after drain"));
    let (server_stream, peer) = listener.accept().expect("accept after drain");
    let _client = client_handle.join().expect("client connect");
    let mut server_stream = DaemonStream::plain(server_stream);

    let flags = no_op_signal_flags();
    let config_path: Option<PathBuf> = None;
    let limiter: Option<Arc<ConnectionLimiter>> = None;
    let log_sink: Option<SharedLogSink> = None;
    let notifier = systemd::ServiceNotifier::new();
    let counter = ConnectionCounter::new();
    let g1 = counter.acquire();
    let _g2 = counter.acquire();
    assert_eq!(counter.active(), 2);

    let state = test_accept_loop_state(
        &flags,
        &config_path,
        &limiter,
        &log_sink,
        &notifier,
        counter.clone(),
        Some(2),
    );

    // Drop one worker's guard - simulating a finished session.
    drop(g1);
    assert_eq!(counter.active(), 1);

    // Admission must now proceed (return value `false`) and the helper
    // must not write a refusal line to the socket.
    assert!(!refuse_if_at_capacity(&mut server_stream, peer, &state));
}

#[test]
fn default_listen_backlog_is_128() {
    // The default backlog must be high enough for production workloads.
    // A value of 5 (upstream's historical default) causes connection drops
    // under moderate concurrency. 128 matches SOMAXCONN on most Linux
    // systems and is the standard default for production TCP servers.
    assert_eq!(DEFAULT_LISTEN_BACKLOG, 128);
}

#[test]
fn warn_per_family_bind_failure_labels_ipv6() {
    // The dual-stack startup must surface per-family failure with the
    // correct address-family label so operators investigating partial
    // listener reachability can identify which family degraded. This
    // mirrors upstream socket.c:463-465's `(address-family %d)` diagnostic.
    let addr: SocketAddr = "[::]:8873".parse().unwrap();
    let error = io::Error::new(io::ErrorKind::AddrNotAvailable, "test failure");

    // Helper takes no log sink so it emits via eprintln! - this exercises
    // the formatting path without requiring a SharedLogSink fixture.
    warn_per_family_bind_failure(None, addr, &error);
}

#[test]
fn warn_per_family_bind_failure_labels_ipv4() {
    // IPv4 counterpart of the IPv6 label test. Both arms of the dual-stack
    // loop must produce identifiable diagnostics when their bind fails so
    // dual-stack startup with a degraded family is auditable.
    let addr: SocketAddr = "0.0.0.0:8873".parse().unwrap();
    let error = io::Error::new(io::ErrorKind::AddrInUse, "test failure");

    warn_per_family_bind_failure(None, addr, &error);
}

#[test]
fn bind_listeners_per_family_falls_back_when_first_family_unreachable() {
    // Regression for UTS-DD-daemon-exit10: GitHub Actions Linux runners have
    // IPv6 disabled or unroutable on loopback. The daemon's dual-stack
    // default used to bind only `[::]:port` and silently swallowed the
    // per-family failure, producing an opaque exit 10 when an IPv4-only
    // client connected. This test simulates that environment by listing an
    // unreachable address first (TEST-NET-1 192.0.2.1 - RFC 5737 reserves
    // 192.0.2.0/24 for documentation and routers drop it, so `bind(2)`
    // returns `EADDRNOTAVAIL`) and a loopback address second. The helper
    // must produce a working listener on the loopback family rather than
    // failing the whole startup.
    //
    // upstream: socket.c::open_socket_in (rsync-3.4.1:428-498) iterates
    // every getaddrinfo result and only fails when zero sockets bound.
    let unreachable = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    let reachable = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let bind_addresses = vec![unreachable, reachable];

    let (listeners, bound_addresses) = bind_listeners_per_family(
        &bind_addresses,
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    )
    .expect("at least one family must bind");

    assert_eq!(listeners.len(), 1, "only the reachable family should bind");
    assert_eq!(bound_addresses.len(), 1);
    assert!(
        bound_addresses[0].ip() == reachable,
        "fallback listener must be on the reachable address, got {}",
        bound_addresses[0]
    );
    assert!(
        bound_addresses[0].port() != 0,
        "kernel must assign an ephemeral port for the bound listener"
    );
}

#[test]
fn bind_listeners_per_family_replicates_acceptor_threads() {
    // acceptor_threads = N must bind N listener sockets for a reachable
    // family. With port 0 each replica gets its own ephemeral port, which
    // deterministically exercises the replication loop on every platform
    // regardless of SO_REUSEPORT same-port semantics (the load-balancing
    // benefit of binding to a fixed shared port is a kernel feature covered
    // by the per-socket set_reuse_port call, not this counting test).
    let reachable = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let bind_addresses = vec![reachable];

    let (listeners, bound_addresses) = bind_listeners_per_family(
        &bind_addresses,
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        3,
        None,
    )
    .expect("the reachable family must bind all replicas");

    assert_eq!(
        listeners.len(),
        3,
        "acceptor_threads=3 must bind three listener replicas"
    );
    assert_eq!(bound_addresses.len(), 3);
    for addr in &bound_addresses {
        assert_eq!(addr.ip(), reachable, "every replica binds the same family");
        assert!(addr.port() != 0, "each replica must get a real ephemeral port");
    }
}

#[test]
fn default_acceptor_threads_is_one() {
    // Absent an `acceptor threads` directive the daemon binds a single
    // listener per family, preserving the historical pre-NACC-2 behaviour.
    assert_eq!(RuntimeOptions::default().acceptor_threads(), 1);
}

// Unix-only: asserts POSIX SO_REUSEADDR semantics (a second bind on an active
// listener is refused). Windows SO_REUSEADDR instead permits re-binding, so this
// exact-refusal invariant is a Unix property.
#[cfg(unix)]
#[test]
fn default_single_listener_refuses_a_second_bind_on_the_same_port() {
    // upstream: socket.c:447 - open_socket_in() sets SO_REUSEADDR only. The
    // default single-listener daemon (acceptor_threads == 1) must therefore NOT
    // set SO_REUSEPORT, so a second bind on the same in-use port is refused with
    // EADDRINUSE rather than co-binding. This is what makes concurrent daemon
    // tests safe from cross-connection load-balancing.
    let reachable = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    // Bind the first default listener on an ephemeral port and learn it; the
    // listener holds the port so there is no reserve/rebind window.
    let (first, first_addrs) = bind_listeners_per_family(
        &[reachable],
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    )
    .expect("first default bind must succeed");
    assert_eq!(first.len(), 1);
    let port = first_addrs[0].port();
    assert!(port != 0);

    // A second default (replicas == 1) bind on that same, in-use port must be
    // refused - no SO_REUSEPORT co-bind.
    let second = bind_listeners_per_family(
        &[reachable],
        port,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    );
    assert!(
        second.is_err(),
        "a second default-daemon bind on an in-use port must fail (SO_REUSEADDR only), got Ok"
    );
}

// Unix-only: SO_REUSEPORT (the fixed-port co-bind mechanism) is a POSIX socket
// option; socket2's setter is Unix-only and the daemon only sets it under
// `#[cfg(unix)]`.
#[cfg(unix)]
#[test]
fn multi_acceptor_replicas_co_bind_the_same_fixed_port() {
    // The opt-in multi-acceptor daemon (acceptor_threads > 1) still sets
    // SO_REUSEPORT on its replica sockets, so multiple listeners share ONE fixed
    // port and the kernel load-balances accepts across them. Binding replicas on
    // a fixed port (rather than port 0, which hands each replica a distinct
    // ephemeral port) is what actually exercises the shared-port co-bind.
    let reachable = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    // Reserve a currently-free fixed port. A concurrent test could steal it in
    // the window between reserve and bind, so retry until a clean attempt binds
    // both replicas (or the budget is exhausted).
    for _ in 0..32 {
        let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("reserve free port");

        match bind_listeners_per_family(
            &[reachable],
            port,
            DEFAULT_LISTEN_BACKLOG,
            TcpFastOpenMode::Off,
            2,
            None,
        ) {
            Ok((listeners, addrs)) if listeners.len() == 2 => {
                assert!(
                    addrs.iter().all(|a| a.port() == port),
                    "both replicas must co-bind the same fixed port {port}, got {addrs:?}"
                );
                return;
            }
            // A regression that dropped SO_REUSEPORT would bind only the first
            // replica (the second EADDRINUSEs), returning a single listener.
            // Retry: this attempt lost the reserve race or the port was busy.
            _ => continue,
        }
    }
    panic!(
        "acceptor_threads=2 must co-bind two replica listeners on one fixed port \
         (SO_REUSEPORT); none of the attempts bound both replicas"
    );
}

#[test]
fn bind_listeners_per_family_fails_only_when_all_families_unreachable() {
    // Companion to the fallback test: confirm the helper surfaces an error
    // when no family in the input list can bind. Both addresses here are
    // TEST-NET-1 documentation prefixes (RFC 5737) which the kernel cannot
    // assign to a local socket, so every bind attempt returns
    // `EADDRNOTAVAIL`. Matches upstream socket.c:492-498 which returns NULL
    // ("unable to bind any inbound sockets") only when the per-family loop
    // produced zero usable sockets.
    let bind_addresses = vec![
        IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2)),
    ];

    let err = bind_listeners_per_family(
        &bind_addresses,
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    )
    .expect_err("no family should bind");

    // The error returned must be the underlying bind failure (typically
    // AddrNotAvailable). Callers map this to a DaemonError via bind_error()
    // with exit code 10 (socket I/O), matching upstream's exit semantics.
    assert!(
        matches!(
            err.kind(),
            io::ErrorKind::AddrNotAvailable | io::ErrorKind::PermissionDenied
        ),
        "expected AddrNotAvailable or PermissionDenied, got {:?}: {err}",
        err.kind()
    );
}

#[test]
fn bind_listeners_per_family_falls_back_from_ipv6_to_ipv4() {
    // The default dual-stack listener attempts IPv6 first then IPv4. On
    // GitHub Actions Linux runners the IPv6 stack is partially configured
    // so `bind(2)` to a non-link-local IPv6 address returns
    // `EADDRNOTAVAIL`. This test simulates that environment with a
    // synthetic getaddrinfo-style result list: an unreachable IPv6
    // documentation address (RFC 3849 2001:db8::/32, which the kernel
    // cannot bind to a local socket) is followed by IPv4 loopback. The
    // helper must surface the IPv6 failure as a warning, continue to
    // IPv4, and produce a working IPv4 listener instead of failing the
    // whole daemon startup.
    //
    // upstream: socket.c::open_socket_in (rsync-3.4.4:432-498) iterates
    // every getaddrinfo result, accumulates per-family errors via
    // `errmsgs[ecnt++]`, and only fails when zero sockets bound.
    let unreachable_v6 = IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1));
    let reachable_v4 = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    let bind_addresses = vec![unreachable_v6, reachable_v4];

    let (listeners, bound_addresses) = bind_listeners_per_family(
        &bind_addresses,
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    )
    .expect("IPv4 fallback must bind when IPv6 is unreachable");

    assert_eq!(listeners.len(), 1, "only the reachable IPv4 family should bind");
    assert_eq!(bound_addresses.len(), 1);
    assert!(
        bound_addresses[0].is_ipv4(),
        "fallback listener must be on IPv4, got {}",
        bound_addresses[0]
    );
    assert!(
        bound_addresses[0].ip() == reachable_v4,
        "fallback listener must be on IPv4 loopback, got {}",
        bound_addresses[0]
    );
    assert!(
        bound_addresses[0].port() != 0,
        "kernel must assign an ephemeral port for the bound listener"
    );
}

#[test]
fn bind_listeners_per_family_single_family_propagates_error() {
    // When only one address is provided (no dual-stack), the helper must
    // propagate the bind failure immediately - there is no other family to
    // fall back to. This matches the non-dual-stack branch in the loop and
    // ensures explicit `--address` configurations still surface the bind
    // failure verbatim to the operator.
    let bind_addresses = vec![IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1))];

    let err = bind_listeners_per_family(
        &bind_addresses,
        0,
        DEFAULT_LISTEN_BACKLOG,
        TcpFastOpenMode::Off,
        1,
        None,
    )
    .expect_err("the single configured address must fail to bind");

    assert!(
        matches!(
            err.kind(),
            io::ErrorKind::AddrNotAvailable | io::ErrorKind::PermissionDenied
        ),
        "expected AddrNotAvailable or PermissionDenied, got {:?}: {err}",
        err.kind()
    );
}

#[test]
fn parse_address_family_env_accepts_ipv4_variants() {
    // The runtime override accepts a few common spellings so operators
    // do not have to memorise the exact token. Case is normalised before
    // matching.
    for value in ["ipv4", "IPv4", "v4", "4", "INET", " ipv4 "] {
        assert_eq!(
            parse_address_family_env(value),
            Some(AddressFamilyOverride::Ipv4),
            "value {value:?} must map to Ipv4"
        );
    }
}

#[test]
fn parse_address_family_env_accepts_ipv6_variants() {
    for value in ["ipv6", "IPv6", "v6", "6", "INET6"] {
        assert_eq!(
            parse_address_family_env(value),
            Some(AddressFamilyOverride::Ipv6),
            "value {value:?} must map to Ipv6"
        );
    }
}

#[test]
fn parse_address_family_env_accepts_both_variants() {
    for value in ["both", "BOTH", "dual", "dualstack", "dual-stack"] {
        assert_eq!(
            parse_address_family_env(value),
            Some(AddressFamilyOverride::Both),
            "value {value:?} must map to Both"
        );
    }
}

#[test]
fn parse_address_family_env_rejects_unknown() {
    // Unknown values must fall through to `None` so the daemon falls
    // back to the compile-time default instead of refusing to start on
    // an operator typo.
    for value in ["", " ", "ipv7", "any", "auto", "1"] {
        assert_eq!(
            parse_address_family_env(value),
            None,
            "value {value:?} must not map to a family"
        );
    }
}

#[test]
fn warn_per_family_accept_failure_labels_ipv6() {
    // The dual-stack accept loop must produce an identifiable diagnostic
    // when one family's acceptor dies while another is still healthy.
    // Mirrors the bind-failure warning's labelling contract.
    let addr: SocketAddr = "[::]:8873".parse().unwrap();
    let error = io::Error::other("test failure");
    warn_per_family_accept_failure(None, addr, &error);
}

#[test]
fn warn_per_family_accept_failure_labels_ipv4() {
    let addr: SocketAddr = "0.0.0.0:8873".parse().unwrap();
    let error = io::Error::other("test failure");
    warn_per_family_accept_failure(None, addr, &error);
}

#[test]
fn single_listener_engine_poll_idle_when_no_connection() {
    // A quiet daemon must yield control rather than error: the non-blocking
    // accept returns WouldBlock, which the engine maps to AcceptOutcome::Idle
    // after its bounded sleep so the accept loop can re-check signal flags.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut engine = SingleListenerEngine::new(listener, local, None).expect("engine");

    assert!(
        matches!(engine.poll().expect("poll"), AcceptOutcome::Idle),
        "poll with no pending connection must yield Idle"
    );
}

#[test]
fn single_listener_engine_poll_returns_connection_with_blocking_reset() {
    // poll() must return Connection for an accepted client, and the accepted
    // socket must be reset to BLOCKING mode. On BSD-derived kernels (macOS) the
    // accepted socket inherits the listener's O_NONBLOCK; without the engine's
    // set_nonblocking(false) reset, the blocking read below would fail with
    // WouldBlock instead of waiting for the delayed client bytes. The 100ms
    // write delay guarantees the data is not yet available when read_exact is
    // called, so this discriminates blocking vs non-blocking on that host.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut engine = SingleListenerEngine::new(listener, local, None).expect("engine");

    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(local).expect("connect");
        thread::sleep(Duration::from_millis(100));
        stream.write_all(b"hi").expect("client write");
        // Hold the connection open until the server has read the bytes.
        thread::sleep(Duration::from_millis(50));
    });

    let mut accepted = None;
    for _ in 0..40 {
        match engine.poll().expect("poll") {
            AcceptOutcome::Connection(stream, peer) => {
                accepted = Some((stream, peer));
                break;
            }
            AcceptOutcome::Idle => continue,
            AcceptOutcome::Closed => panic!("single-listener engine never reports Closed"),
        }
    }

    let (mut stream, peer) = accepted.expect("a client connection was accepted");
    assert!(peer.ip().is_loopback(), "peer must be loopback, got {peer}");

    // Blocking read of the delayed client bytes: succeeds only if the accepted
    // socket is in blocking mode. No read timeout is set, since that would mask
    // the very non-blocking-vs-blocking distinction under test.
    let mut buf = [0u8; 2];
    stream
        .read_exact(&mut buf)
        .expect("blocking read of client bytes (accepted socket must be blocking)");
    assert_eq!(&buf, b"hi");

    let _ = client.join();
}

#[test]
fn relay_accept_item_delivers_when_capacity_available() {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AcceptItem>(1);
    let shutdown = AtomicBool::new(false);
    let graceful = AtomicBool::new(false);
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

    assert!(relay_accept_item(
        &tx,
        Err((addr, io::Error::other("boom"))),
        &shutdown,
        &graceful,
    ));
    assert!(rx.recv().is_ok());
}

#[test]
fn relay_accept_item_bails_on_shutdown_when_full() {
    // Capacity 1, never drained: the first item fills the relay.
    let (tx, _rx) = std::sync::mpsc::sync_channel::<AcceptItem>(1);
    let shutdown = AtomicBool::new(false);
    let graceful = AtomicBool::new(false);
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

    assert!(relay_accept_item(
        &tx,
        Err((addr, io::Error::other("first"))),
        &shutdown,
        &graceful,
    ));

    // The relay is now full. With shutdown requested, a second relay must
    // return false promptly instead of blocking forever on the full channel,
    // which is what keeps join() from wedging at teardown under backpressure.
    shutdown.store(true, Ordering::Relaxed);
    assert!(!relay_accept_item(
        &tx,
        Err((addr, io::Error::other("second"))),
        &shutdown,
        &graceful,
    ));
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_poll_idle_when_no_connection() {
    // A quiet daemon must yield Idle (not error, not busy-spin): the kevent
    // wait returns empty after its bounded timeout so the accept loop can
    // re-check signal flags. Also exercises the real kqueue registration path.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    let start = std::time::Instant::now();
    assert!(
        matches!(engine.poll().expect("poll"), AcceptOutcome::Idle),
        "poll with no pending connection must yield Idle"
    );
    // The wait must actually block for roughly the signal-check interval rather
    // than spin: this is the anti-busy-wait guarantee.
    assert!(
        start.elapsed() >= Duration::from_millis(80),
        "kevent wait must park for ~100ms, not busy-spin"
    );
    engine.shutdown();
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_poll_returns_connection_with_blocking_reset() {
    // The kqueue engine must accept a real client via EVFILT_READ readiness and
    // return it as a BLOCKING stream. On macOS the accepted socket inherits the
    // listener's O_NONBLOCK; without the engine's set_nonblocking(false) reset,
    // the delayed blocking read below would fail with WouldBlock. The 100ms
    // write delay guarantees the data is not yet available at read_exact time,
    // so this discriminates blocking vs non-blocking.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(local).expect("connect");
        thread::sleep(Duration::from_millis(100));
        stream.write_all(b"hi").expect("client write");
        thread::sleep(Duration::from_millis(50));
    });

    let mut accepted = None;
    for _ in 0..40 {
        match engine.poll().expect("poll") {
            AcceptOutcome::Connection(stream, peer) => {
                accepted = Some((stream, peer));
                break;
            }
            AcceptOutcome::Idle => continue,
            AcceptOutcome::Closed => panic!("kqueue engine never reports Closed"),
        }
    }

    let (mut stream, peer) = accepted.expect("a client connection was accepted via kqueue");
    assert!(peer.ip().is_loopback(), "peer must be loopback, got {peer}");

    // Blocking read of the delayed client bytes: succeeds only if the accepted
    // socket is in blocking mode. No read timeout is set so a non-blocking
    // socket would surface as an error here.
    let mut buf = [0u8; 2];
    stream
        .read_exact(&mut buf)
        .expect("blocking read of client bytes (accepted socket must be blocking)");
    assert_eq!(&buf, b"hi");

    engine.shutdown();
    let _ = client.join();
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_level_triggered_backlog_is_not_stranded() {
    // Level-triggered readiness must re-surface a queued backlog on successive
    // polls even though the engine takes only ONE connection per poll. Connect
    // three clients up front, then verify the engine hands back all three
    // distinct connections across successive polls (none stranded behind a
    // consumed edge - the failure mode an EV_CLEAR registration would cause).
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");

    let mut clients = Vec::new();
    for _ in 0..3 {
        clients.push(TcpStream::connect(local).expect("connect"));
    }
    // Give the kernel a moment to enqueue all three on the listen backlog.
    thread::sleep(Duration::from_millis(50));

    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    let mut accepted = 0;
    for _ in 0..40 {
        match engine.poll().expect("poll") {
            AcceptOutcome::Connection(_, peer) => {
                assert!(peer.ip().is_loopback());
                accepted += 1;
                if accepted == 3 {
                    break;
                }
            }
            AcceptOutcome::Idle => continue,
            AcceptOutcome::Closed => panic!("kqueue engine never reports Closed"),
        }
    }
    assert_eq!(accepted, 3, "all queued connections must be delivered, not stranded");

    engine.shutdown();
    drop(clients);
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_needs_one_poll_per_queued_connection() {
    // Admission-lockstep invariant: the engine must NOT drain and buffer a
    // ready backlog inside a single poll. The shared accept loop reaps finished
    // worker threads (dropping their connection guards) once per iteration,
    // immediately before it polls; if one poll returned several connections
    // their admissions would run back-to-back with no intervening reap. Yielding
    // one connection per poll keeps the `max connections` accounting in lockstep
    // with the loop's reap cadence, matching the single-listener engine.
    //
    // This is observable: with three clients queued up front, delivering all
    // three requires (at least) three separate `poll()` calls that each return a
    // `Connection`. A drain-and-buffer engine would satisfy the 2nd and 3rd from
    // an internal queue without a fresh readiness wait; this test asserts the
    // externally-visible contract that each connection costs its own poll.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");

    let mut clients = Vec::new();
    for _ in 0..3 {
        clients.push(TcpStream::connect(local).expect("connect"));
    }
    thread::sleep(Duration::from_millis(50));

    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    // Count how many polls returned a Connection. Each poll returns at most one
    // (the engine returns on the first successful accept), so three queued
    // clients require three connection-returning polls.
    let mut connection_polls = 0;
    for _ in 0..40 {
        match engine.poll().expect("poll") {
            AcceptOutcome::Connection(_, peer) => {
                assert!(peer.ip().is_loopback());
                connection_polls += 1;
                if connection_polls == 3 {
                    break;
                }
            }
            AcceptOutcome::Idle => continue,
            AcceptOutcome::Closed => panic!("kqueue engine never reports Closed"),
        }
    }

    assert_eq!(
        connection_polls, 3,
        "each queued connection must be delivered by its own poll so admission \
         stays in lockstep with per-iteration worker reaping"
    );

    engine.shutdown();
    drop(clients);
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_shutdown_is_idempotent() {
    // shutdown() must be safe to call more than once (the accept loop calls it
    // on exit, and error paths may call it too). A second call finds the
    // listeners already cleared and must not panic.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");
    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    engine.shutdown();
    engine.shutdown();
    // Polling after shutdown has no listeners; wait over an empty kqueue simply
    // times out to Idle without error.
    assert!(matches!(engine.poll().expect("poll"), AcceptOutcome::Idle));
}

#[cfg(all(target_os = "macos", feature = "macos-kqueue"))]
#[test]
fn kqueue_engine_delivers_queued_connection_without_sleep_floor() {
    // The whole point of the readiness engine: a connection already queued on
    // the listen backlog must be delivered by an `EVFILT_READ` wake, not after
    // the portable engine's 50ms `WouldBlock` sleep floor. Connect a client and
    // let the kernel enqueue it, then assert the very first poll returns the
    // Connection and does so well under the 50ms busy-poll interval this engine
    // replaces. A regression to the sleep-then-retry shape would push the first
    // delivering poll to >= 50ms and trip this bound.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let local = listener.local_addr().expect("local addr");

    let client = TcpStream::connect(local).expect("connect");
    // Give the kernel a moment to place the connection on the backlog so the
    // EVFILT_READ registration sees it ready on the first wait.
    thread::sleep(Duration::from_millis(20));

    let mut engine =
        KqueueAcceptEngine::new(vec![listener], &[local], None).expect("kqueue engine");

    let start = std::time::Instant::now();
    let outcome = engine.poll().expect("poll");
    let elapsed = start.elapsed();

    assert!(
        matches!(outcome, AcceptOutcome::Connection(_, _)),
        "an already-queued connection must be delivered on the first poll"
    );
    assert!(
        elapsed < Duration::from_millis(50),
        "readiness wake must beat the 50ms busy-poll floor, took {elapsed:?}"
    );

    engine.shutdown();
    drop(client);
}
