#[test]
fn run_daemon_records_log_file_entries() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let temp = tempdir().expect("log dir");
    let log_path = temp.path().join("rsyncd.log");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs"),
            OsString::from("--once"),
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

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("module acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("module response");
    assert!(line.starts_with("@ERROR:"));

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    assert!(log_contents.contains("connect from"));
    assert!(log_contents.contains("127.0.0.1"));
    assert!(log_contents.contains("module 'docs'"));
}

