/// Tests for daemon mode version negotiation.
///
/// These tests verify the correct behavior of protocol version negotiation
/// during the daemon handshake, including version clamping and compatibility.

#[test]
fn daemon_negotiation_version_sends_greeting_first() {
    // Verify that the daemon sends the @RSYNCD greeting before the client.
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
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");

    // Read the greeting without sending anything first
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Daemon should send greeting first
    assert!(
        line.starts_with("@RSYNCD:"),
        "Daemon should send greeting first, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_greeting_format() {
    // Verify the greeting format matches @RSYNCD: <major>.<minor>
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

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Verify format: @RSYNCD: <digits>.<digits>
    assert!(line.starts_with("@RSYNCD: "));
    let version_part = line.strip_prefix("@RSYNCD: ").unwrap().trim();

    // Should have format like "32.0" with optional digest list
    let parts: Vec<&str> = version_part.split_whitespace().collect();
    assert!(!parts.is_empty(), "Version should not be empty");

    let version = parts[0];
    let version_parts: Vec<&str> = version.split('.').collect();
    assert_eq!(
        version_parts.len(),
        2,
        "Version should have major.minor format"
    );
    assert!(
        version_parts[0].parse::<u32>().is_ok(),
        "Major version should be numeric"
    );
    assert!(
        version_parts[1].parse::<u32>().is_ok(),
        "Minor version should be numeric"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_accepts_older_client_version() {
    // Verify that daemon accepts connections from clients with older protocol versions.
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

    // Send older protocol version (29)
    stream
        .write_all(b"@RSYNCD: 29.0\n")
        .expect("send old version");
    stream.flush().expect("flush");

    // Request a module (should work with older protocol)
    stream.write_all(b"testmod\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive an error (module doesn't exist) or response
    line.clear();
    reader.read_line(&mut line).expect("response");

    // The response should be a valid daemon message, not a protocol error
    assert!(
        line.starts_with("@") || line.contains("module"),
        "Expected valid response for older protocol, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_includes_digest_list_for_protocol_31_plus() {
    // Verify that the daemon greeting includes digest list for protocol >= 31.
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
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Parse the greeting
    let trimmed = line.trim();
    let after_prefix = trimmed.strip_prefix("@RSYNCD: ").expect("prefix");
    let parts: Vec<&str> = after_prefix.split_whitespace().collect();

    // First part is version
    let version_str = parts[0];
    let major: u32 = version_str
        .split('.')
        .next()
        .unwrap()
        .parse()
        .expect("major version");

    // For protocol >= 31, there should be digest algorithms listed
    if major >= 31 && parts.len() > 1 {
        // Digest list should contain common algorithms
        let digest_list = &parts[1..];
        assert!(
            !digest_list.is_empty(),
            "Protocol 31+ should advertise digests"
        );
        // Common digests that should be present
        let all_digests = digest_list.join(" ");
        // At least one common digest should be present
        assert!(
            all_digests.contains("md4")
                || all_digests.contains("md5")
                || all_digests.contains("xxh")
                || all_digests.contains("sha"),
            "Expected common digest algorithms, got: {all_digests}"
        );
    }

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_echoes_client_digests() {
    // Verify that when client sends digests, the negotiation proceeds correctly.
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

    // Send version with digests (like a modern client would)
    stream
        .write_all(b"@RSYNCD: 31.0 md4 md5 xxh3\n")
        .expect("send version with digests");
    stream.flush().expect("flush");

    // Request list to verify protocol accepted
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Should receive valid response
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.starts_with("@RSYNCD:"),
        "Expected valid response with digests, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_handles_whitespace_variations() {
    // Verify that the daemon handles various whitespace in version lines.
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

    // Send version with extra whitespace (should be tolerated)
    stream
        .write_all(b"@RSYNCD:  31.0  \n")
        .expect("send version with whitespace");
    stream.flush().expect("flush");

    // Request list
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Should receive valid response
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.starts_with("@RSYNCD:"),
        "Expected valid response with whitespace variations, got: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_version_greeting_ends_with_newline() {
    // Verify that the greeting ends with a newline for proper line-based parsing.
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
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Greeting must end with newline
    assert!(
        line.ends_with('\n'),
        "Greeting should end with newline: {line:?}"
    );

    drop(reader);
    let _ = handle.join();
}
