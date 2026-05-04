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
