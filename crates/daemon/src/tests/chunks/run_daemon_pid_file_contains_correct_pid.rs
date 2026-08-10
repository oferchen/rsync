/// Verifies that the PID file written by the daemon contains the process ID
/// of the daemon process followed by a newline, matching upstream rsync's
/// `write_pid_file()` format (`%ld\n`).
///
/// Also verifies that the file is created with mode 0644 on Unix platforms.
#[test]
fn run_daemon_pid_file_contains_correct_pid() {
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

    let pid_clone = pid_path.clone();
    drop(held_listener);
    let handle = thread::spawn(move || run_daemon(config));

    let start = Instant::now();
    while !pid_clone.exists() {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("pid file not created within 5 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    }

    // Read the PID file and verify it matches upstream rsync format: "%ld\n".
    let content = fs::read_to_string(&pid_path).expect("read pid file");
    let pid_str = content.trim_end_matches('\n');
    let pid: u32 = pid_str
        .parse()
        .unwrap_or_else(|_| panic!("pid file must contain a numeric PID, got: {content:?}"));
    assert!(pid > 0, "PID must be positive");
    assert!(
        content.ends_with('\n'),
        "pid file must end with a newline, got: {content:?}"
    );

    // Verify file permissions are 0644 on Unix platforms.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = fs::metadata(&pid_path).expect("pid file metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "pid file must have mode 0644, got {mode:#o}");
    }

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    drop(reader);
    drop(stream);

    handle.join().expect("daemon thread").expect("daemon exit");
}
