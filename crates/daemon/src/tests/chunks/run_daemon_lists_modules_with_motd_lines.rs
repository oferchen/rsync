#[test]
fn run_daemon_lists_modules_with_motd_lines() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(
        &motd_path,
        "Welcome to rsyncd\nRemember to sync responsibly\n",
    )
    .expect("write motd");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--motd-file"),
            motd_path.as_os_str().to_os_string(),
            OsString::from("--motd-line"),
            OsString::from("Additional notice"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("motd line 1");
    assert_eq!(line.trim_end(), "@RSYNCD: MOTD Welcome to rsyncd");

    line.clear();
    reader.read_line(&mut line).expect("motd line 2");
    assert_eq!(
        line.trim_end(),
        "@RSYNCD: MOTD Remember to sync responsibly"
    );

    line.clear();
    reader.read_line(&mut line).expect("motd line 3");
    assert_eq!(line.trim_end(), "@RSYNCD: MOTD Additional notice");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("module line");
    assert_eq!(line.trim_end(), "docs");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

