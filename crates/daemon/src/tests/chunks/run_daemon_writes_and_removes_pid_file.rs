#[test]
fn run_daemon_writes_and_removes_pid_file() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let temp = tempdir().expect("pid dir");
    let pid_path = temp.path().join("rsyncd.pid");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--pid-file"),
            pid_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let handle = spawn_daemon(config, held_listener);

    // The daemon creates the PID file before entering accept(), so receiving
    // the greeting proves the file is already on disk without polling or sleeps.
    let mut stream = connect_client(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);
    assert!(pid_path.exists(), "pid file must exist while daemon serves");

    send_client_greeting(&mut stream);
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    assert!(!pid_path.exists());
}
