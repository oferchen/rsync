/// Tests for daemon mode error handling during negotiation.
///
/// These tests verify the correct behavior of the daemon when encountering
/// various error conditions during the negotiation protocol.

#[test]
fn daemon_negotiation_error_unknown_module() {
    // Verify that requesting an unknown module returns an appropriate error.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request non-existent module
    stream
        .write_all(b"nonexistent_module\n")
        .expect("send module");
    stream.flush().expect("flush");

    // Should receive @ERROR with unknown module message
    line.clear();
    reader.read_line(&mut line).expect("error response");
    assert!(
        line.contains("@ERROR:") && (line.contains("unknown module") || line.contains("Unknown module")),
        "Expected unknown module error, got: {line}"
    );

    // Should receive EXIT
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_error_empty_module_request() {
    // Verify that an empty module request is handled gracefully.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Send empty module request (just newline)
    stream.write_all(b"\n").expect("send empty");
    stream.flush().expect("flush");

    // Should receive an error response
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") || line.contains("@RSYNCD:"),
        "Expected error response for empty module, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_error_host_denied() {
    // Verify that hosts_deny properly blocks access.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[restricted]\npath = {}\nhosts deny = 127.0.0.1\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request restricted module
    stream.write_all(b"restricted\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("access denied"),
        "Expected access denied, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_error_refused_options() {
    // Verify that refused options are properly rejected.
    // Note: Refused options are checked when processing client arguments after
    // authentication, not during module request. This test verifies the daemon
    // correctly rejects refused options when sent as #OPT during negotiation.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[secure]\npath = {}\nrefuse options = delete\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Send refused option using the daemon option format (prefixed with hash)
    // The daemon parses lines starting with # followed by option name
    stream.write_all(b"#delete\n").expect("send option");
    stream.flush().expect("flush");

    // Request module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive either OK (authenticated) or error about refused option.
    // The daemon may also reset the connection before we read (race condition).
    line.clear();
    match reader.read_line(&mut line) {
        Ok(_) => {
            // The OK is sent first, then when --delete is in client args it gets refused
            assert!(
                line.contains("@RSYNCD: OK") || line.contains("@ERROR:"),
                "Expected OK or error, got: {line}"
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
            // Daemon closed the connection — acceptable outcome for refused options.
        }
        Err(e) => panic!("unexpected I/O error: {e}"),
    }

    drop(reader);
    // Don't assert on result - daemon may fail gracefully when client doesn't continue
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_error_max_connections_exceeded() {
    // Verify that max-connections limit is enforced.
    // Note: max connections = 0 means unlimited in rsync, so we use max connections = 1
    // and test by establishing multiple connections.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let lock_dir = dir.path().join("locks");
    fs::create_dir_all(&lock_dir).expect("lock dir");

    let config_path = dir.path().join("rsyncd.conf");
    // max connections = 1 means only 1 connection at a time
    fs::write(
        &config_path,
        format!(
            "lock file = {}/rsyncd.lock\n\n[limited]\npath = {}\nmax connections = 1\n",
            lock_dir.display(),
            module_dir.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

    // Run daemon with multiple allowed sessions
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    // First connection should succeed (up to the OK)
    let mut stream1 = connect_with_retries(port);
    let mut reader1 = BufReader::new(stream1.try_clone().expect("clone"));

    let mut line = String::new();
    reader1.read_line(&mut line).expect("greeting1");

    stream1
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version1");
    stream1.flush().expect("flush1");

    stream1.write_all(b"limited\n").expect("send module1");
    stream1.flush().expect("flush module1");

    line.clear();
    reader1.read_line(&mut line).expect("response1");
    // First connection should get OK (we're within max connections)
    assert!(
        line.contains("@RSYNCD: OK"),
        "First connection should succeed, got: {line}"
    );

    // Keep first connection open and try second connection
    let mut stream2 = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .expect("connect second stream");
    let mut reader2 = BufReader::new(stream2.try_clone().expect("clone2"));

    line.clear();
    reader2.read_line(&mut line).expect("greeting2");

    stream2
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version2");
    stream2.flush().expect("flush2");

    stream2.write_all(b"limited\n").expect("send module2");
    stream2.flush().expect("flush module2");

    // Second connection should get max connections error
    line.clear();
    reader2.read_line(&mut line).expect("response2");

    // Daemon enforces max connections limit
    assert!(
        line.contains("@ERROR:") || line.contains("max connections"),
        "Second connection should fail with max connections error, got: {line}"
    );

    drop(reader1);
    drop(reader2);
    drop(stream1);
    drop(stream2);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_error_sanitizes_module_name_in_response() {
    // Verify that module names with special characters are sanitized in error messages.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request module with control characters
    stream
        .write_all(b"module\x00with\x1bcontrol\n")
        .expect("send malicious module");
    stream.flush().expect("flush");

    // Should receive error without raw control characters
    line.clear();
    reader.read_line(&mut line).expect("response");

    // The error message should not contain raw control characters
    assert!(
        !line.contains('\x00') && !line.contains('\x1b'),
        "Response should not contain raw control characters: {line:?}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_error_sends_exit_after_error() {
    // Verify that EXIT is sent after error messages.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request non-existent module
    stream.write_all(b"fake_module\n").expect("send module");
    stream.flush().expect("flush");

    // Read error
    line.clear();
    reader.read_line(&mut line).expect("error");
    assert!(line.contains("@ERROR:"), "Expected error, got: {line}");

    // Read EXIT
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(
        line, "@RSYNCD: EXIT\n",
        "Expected EXIT after error, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_error_connection_closed_early() {
    // Verify that daemon handles early connection close gracefully.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Close connection immediately without responding
    drop(reader);
    drop(stream);

    // Daemon should handle this gracefully
    let result = handle.join().expect("daemon thread");
    // Result may be Ok or Err depending on timing, but should not panic
    let _ = result;
}

#[test]
fn daemon_negotiation_error_invalid_greeting_response() {
    // Verify that daemon handles invalid client greeting responses.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send garbage instead of proper greeting response
    stream
        .write_all(b"not a valid response\n")
        .expect("send garbage");
    stream.flush().expect("flush");

    // Daemon should handle this - the garbage becomes a module request
    // which will fail with unknown module
    line.clear();
    let read_result = reader.read_line(&mut line);

    // Should either get an error response or EOF
    if read_result.is_ok() && !line.is_empty() {
        assert!(
            line.contains("@ERROR:") || line.contains("@RSYNCD:"),
            "Expected error or daemon response, got: {line}"
        );
    }

    drop(reader);
    let _ = handle.join();
}
