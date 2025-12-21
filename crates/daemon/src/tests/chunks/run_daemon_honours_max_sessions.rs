#[test]
fn run_daemon_honours_max_sessions() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let expected_greeting = legacy_daemon_greeting();
    for _ in 0..2 {
        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        assert_eq!(line, expected_greeting);

        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send handshake response");
        stream.flush().expect("flush handshake response");

        // Send module name immediately after version exchange (no OK expected yet).
        // The daemon only sends @RSYNCD: OK after the module is selected.
        stream.write_all(b"module\n").expect("send module request");
        stream.flush().expect("flush module request");

        // Now we expect an error because 'module' doesn't exist
        line.clear();
        reader.read_line(&mut line).expect("error message");
        assert!(line.starts_with("@ERROR:"));

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");
    }

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

