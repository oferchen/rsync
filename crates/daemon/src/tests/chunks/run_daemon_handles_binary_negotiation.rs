#[test]
fn run_daemon_handles_binary_negotiation() {
    // This test verifies that daemon always uses Legacy (@RSYNCD) protocol,
    // even when client attempts binary negotiation.
    // The rsync daemon protocol is ALWAYS text-based (@RSYNCD), not binary.

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
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    // Even if client sends binary data, daemon sends @RSYNCD greeting
    let binary_data = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream
        .write_all(&binary_data)
        .expect("send binary data");
    stream.flush().expect("flush");

    // Daemon should send @RSYNCD greeting (text protocol)
    let mut greeting = String::new();
    let mut reader = BufReader::new(&mut stream);
    reader
        .read_line(&mut greeting)
        .expect("read greeting");
    assert!(
        greeting.starts_with("@RSYNCD:"),
        "Expected @RSYNCD greeting, got: {greeting}"
    );

    // Daemon will fail after receiving invalid input (binary instead of @RSYNCD response)
    // but that's expected behavior - the test verifies that daemon sends @RSYNCD first
    let _result = handle.join().expect("daemon thread");
    // Don't assert on result - daemon rightfully fails when client sends invalid data
}

