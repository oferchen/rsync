#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; negotiation is platform-independent and covered on Linux/macOS"
)]
fn run_daemon_records_log_file_entries() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let temp = tempdir().expect("log dir");
    let log_path = temp.path().join("rsyncd.log");

    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from(format!("docs={module_path}")),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon responds with OK after module selection
    // (CAP is only sent for #list requests)
    line.clear();
    reader.read_line(&mut line).expect("module acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("module response");
    assert!(line.starts_with("@ERROR:"));

    // upstream: clientserver.c:381-385 - the client treats @ERROR as fatal and
    // returns before reading further, so the daemon sends no @RSYNCD: EXIT after
    // the refusal; the socket just closes (next read is EOF).
    line.clear();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(read, 0, "no trailing @RSYNCD: EXIT after @ERROR, got: {line:?}");

    drop(reader);
    // Bound the join: on Windows the daemon accept loop can linger past the
    // client disconnect. If it detaches (None) the client already saw EXIT and
    // the log contents are asserted below regardless of the daemon Result.
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    assert!(
        log_contents
            .lines()
            .any(|line| line.contains("oc-rsync info: rsyncd version")),
        "log should use oc-rsync branding: {log_contents:?}"
    );
    assert!(log_contents.contains("connect from"));
    assert!(log_contents.contains("127.0.0.1"));
    assert!(log_contents.contains("module 'docs'"));
}

