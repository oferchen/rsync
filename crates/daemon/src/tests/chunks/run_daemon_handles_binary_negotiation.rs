#[test]
fn run_daemon_handles_binary_negotiation() {
    // This test verifies that daemon always uses Legacy (@RSYNCD) protocol,
    // even when client attempts binary negotiation.
    // The rsync daemon protocol is ALWAYS text-based (@RSYNCD), not binary.

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    // Even if client sends binary data, daemon sends @RSYNCD greeting
    let binary_data = u32::from(ProtocolVersion::NEWEST.as_u8()).to_le_bytes();
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
    // but that's expected behavior - the test verifies that daemon sends @RSYNCD first.
    //
    // The daemon reads the client's greeting line with an unbounded, timeout-free
    // read (io_timeout=0, upstream-faithful), so its worker thread only returns on a
    // newline or EOF. The client sent 4 bytes with no newline, so the thread exits
    // only once every client handle is closed. `reader` still borrows `stream`, so
    // drop it and shut the socket down before joining, otherwise the daemon blocks
    // forever waiting for input the client never sends (a hang seen on Windows, where
    // a lingering handle keeps the connection from reaching EOF).
    drop(reader);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    // Bounded join (join-then-detach) like the sibling negotiation tests, so a
    // wedged daemon thread can never turn into a job-level 360s hang.
    let _ = finish_daemon(handle);
    // Don't assert on result - daemon rightfully fails when client sends invalid data
}

