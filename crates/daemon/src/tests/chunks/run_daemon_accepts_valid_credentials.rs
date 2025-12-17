#[test]
fn run_daemon_accepts_valid_credentials() {
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
    fs::write(
        &config_path,
        format!(
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
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
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    line.clear();
    reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    stream.write_all(b"secure\n").expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon responds directly with AUTHREQD for protected modules
    // (CAP is only sent for #list requests)
    line.clear();
    reader.read_line(&mut line).expect("auth request");
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");

    let mut hasher = Md5::new();
    hasher.update(b"password");
    hasher.update(challenge.as_bytes());
    let digest = STANDARD_NO_PAD.encode(hasher.finalize());
    let response_line = format!("alice {digest}\n");
    stream
        .write_all(response_line.as_bytes())
        .expect("send credentials");
    stream.flush().expect("flush credentials");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("post-auth acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    // After successful authentication, the daemon starts the file transfer protocol.
    // The server now enters binary protocol mode and waits for the client to send
    // the file list or other transfer data. Since this test only verifies authentication,
    // we close the connection here. The daemon should handle the closed connection
    // gracefully without timing out.
    drop(stream);
    drop(reader);

    // Verify the daemon thread completes successfully (no panic or timeout)
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should handle connection close gracefully");
}

