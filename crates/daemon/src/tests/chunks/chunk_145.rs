#[test]
fn run_daemon_writes_and_removes_pid_file() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

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

    let pid_clone = pid_path.clone();
    let handle = thread::spawn(move || run_daemon(config));

    let start = Instant::now();
    while !pid_clone.exists() {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("pid file not created");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    assert!(!pid_path.exists());
}

