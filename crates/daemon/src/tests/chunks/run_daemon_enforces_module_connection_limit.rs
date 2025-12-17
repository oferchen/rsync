#[test]
fn run_daemon_enforces_module_connection_limit() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        fs::File::create(&config_path).expect("create config"),
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\nmax connections = 1\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let mut first_stream = connect_with_retries(port);
    let mut first_reader = BufReader::new(first_stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    first_reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    first_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake");
    first_stream.flush().expect("flush handshake");

    line.clear();
    first_reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    first_stream
        .write_all(b"secure\n")
        .expect("send module request");
    first_stream.flush().expect("flush module");

    // Daemon responds directly with AUTHREQD for protected modules
    // (CAP is only sent for #list requests)
    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("auth request for first client");
    assert!(line.starts_with("@RSYNCD: AUTHREQD"));

    let mut second_stream = connect_with_retries(port);
    let mut second_reader = BufReader::new(second_stream.try_clone().expect("clone second"));

    line.clear();
    second_reader.read_line(&mut line).expect("second greeting");
    assert_eq!(line, expected_greeting);

    second_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send second handshake");
    second_stream.flush().expect("flush second handshake");

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    second_stream
        .write_all(b"secure\n")
        .expect("send second module");
    second_stream.flush().expect("flush second module");

    // Daemon responds directly with connection limit error
    // (CAP is only sent for #list requests)
    line.clear();
    second_reader.read_line(&mut line).expect("limit error");
    assert_eq!(
        line.trim_end(),
        "@ERROR: max connections (1) reached -- try again later"
    );

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    first_stream
        .write_all(b"\n")
        .expect("send empty credentials to first client");
    first_stream.flush().expect("flush first credentials");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first denial message");
    assert!(line.starts_with("@ERROR: access denied"));

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(second_reader);
    drop(second_stream);
    drop(first_reader);
    drop(first_stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

