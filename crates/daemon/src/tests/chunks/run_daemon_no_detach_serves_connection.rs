#[test]
fn run_daemon_no_detach_serves_connection() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--no-detach"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    // Send list request to exercise the connection
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    assert!(
        result.is_ok(),
        "daemon with --no-detach should serve a connection successfully"
    );
}
