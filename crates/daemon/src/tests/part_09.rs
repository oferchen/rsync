#[test]
fn run_daemon_filters_modules_during_list_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[public]\npath = /srv/public\n\n[private]\npath = /srv/private\nhosts allow = 10.0.0.0/8\n",
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
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
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("public module");
    assert_eq!(line.trim_end(), "public");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("exit line after accessible modules");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

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

#[test]
fn read_trimmed_line_strips_crlf_terminators() {
    let input: &[u8] = b"payload data\r\n";
    let mut reader = BufReader::new(input);

    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "payload data");

    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}

#[test]
fn version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default()
        .with_daemon_brand(Brand::Upstream)
        .human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::default()
        .with_daemon_brand(Brand::Oc)
        .human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_flag_renders_branded_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn run_daemon_rejects_unknown_argument() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--unknown")])
        .build();

    let error = run_daemon(config).expect_err("unknown argument should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("unsupported daemon argument")
    );
}

#[test]
fn run_daemon_rejects_invalid_port() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--port"), OsString::from("not-a-number")])
        .build();

    let error = run_daemon(config).expect_err("invalid port should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid value for --port")
    );
}

#[test]
fn run_daemon_rejects_invalid_max_sessions() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--max-sessions"), OsString::from("0")])
        .build();

    let error = run_daemon(config).expect_err("invalid max sessions should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("--max-sessions must be greater than zero")
    );
}

#[test]
fn run_daemon_rejects_duplicate_session_limits() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--once"),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let error = run_daemon(config).expect_err("duplicate session limits should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--max-sessions'")
    );
}

#[test]
fn clap_parse_error_is_reported_via_message() {
    let command = clap_command(Brand::Upstream.daemon_program_name());
    let error = command
        .try_get_matches_from(vec!["rsyncd", "--version=extra"])
        .unwrap_err();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from(RSYNCD), OsString::from("--version=extra")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains(error.to_string().trim()));
}

fn connect_with_retries(port: u16) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(20);
    const MAX_BACKOFF: Duration = Duration::from_millis(200);
    const TIMEOUT: Duration = Duration::from_secs(15);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        match TcpStream::connect_timeout(&target, backoff) {
            Ok(stream) => return stream,
            Err(error) => {
                if Instant::now() >= deadline {
                    panic!("failed to connect to daemon within timeout: {error}");
                }

                thread::sleep(backoff);
                backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
            }
        }
    }
}
