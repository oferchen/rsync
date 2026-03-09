#[test]
fn run_daemon_rejects_push_to_read_only_module() {
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
            "[readonly]\npath = {}\nread only = true\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

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

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    // Send client version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // Request the read-only module
    stream
        .write_all(b"readonly\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon sends @RSYNCD: OK after module selection for unauthenticated modules
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Send client arguments that indicate a push (no --sender flag means
    // the server must act as receiver, which conflicts with read-only).
    // upstream: options.c:server_options() — server args are null-terminated
    // for protocol >= 30.
    stream
        .write_all(b"--server\0-logDtpr\0.\0readonly/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    // upstream: clientserver.c — daemon rejects with
    // "@ERROR: module is read only"
    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert_eq!(line.trim_end(), "@ERROR: module is read only");

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
