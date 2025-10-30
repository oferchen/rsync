#[test]
fn run_daemon_enforces_bwlimit_during_module_list() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let mut recorder = rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let port = allocate_test_port();

    let comment = "x".repeat(4096);
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--bwlimit"),
            OsString::from("1K"),
            OsString::from("--module"),
            OsString::from(format!("docs=/srv/docs,{}", comment)),
            OsString::from("--module"),
            OsString::from("logs=/var/log"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    let mut total_bytes = 0usize;

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), format!("docs\t{}", comment));
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line.trim_end(), "logs");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");
    total_bytes += line.len();

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    let recorded = recorder.take();
    assert!(
        !recorded.is_empty(),
        "expected bandwidth limiter to record sleep intervals"
    );
    let total_sleep = recorded
        .into_iter()
        .fold(Duration::ZERO, |acc, duration| acc + duration);
    let expected = Duration::from_secs_f64(total_bytes as f64 / 1024.0);
    let tolerance = Duration::from_millis(250);
    let diff = total_sleep.abs_diff(expected);
    assert!(
        diff <= tolerance,
        "expected sleep around {:?}, got {:?}",
        expected,
        total_sleep
    );
}

