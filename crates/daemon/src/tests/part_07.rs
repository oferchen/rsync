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

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

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

    line.clear();
    reader.read_line(&mut line).expect("unavailable message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: module 'secure' transfers are not yet implemented in this build"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_honours_max_sessions() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let expected_greeting = legacy_daemon_greeting();
    for _ in 0..2 {
        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

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

        stream.write_all(b"module\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("error message");
        assert!(line.starts_with("@ERROR:"));

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");
    }

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_handles_parallel_sessions() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let barrier = Arc::new(Barrier::new(2));
    let mut clients = Vec::new();

    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        clients.push(thread::spawn(move || {
            barrier.wait();
            let mut stream = connect_with_retries(port);
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

            let mut line = String::new();
            reader.read_line(&mut line).expect("greeting");
            assert_eq!(line, legacy_daemon_greeting());

            stream
                .write_all(b"@RSYNCD: 32.0\n")
                .expect("send handshake response");
            stream.flush().expect("flush handshake response");

            line.clear();
            reader.read_line(&mut line).expect("handshake ack");
            assert_eq!(line, "@RSYNCD: OK\n");

            stream.write_all(b"module\n").expect("send module request");
            stream.flush().expect("flush module request");

            line.clear();
            reader.read_line(&mut line).expect("error message");
            assert!(line.starts_with("@ERROR:"));

            line.clear();
            reader.read_line(&mut line).expect("exit message");
            assert_eq!(line, "@RSYNCD: EXIT\n");
        }));
    }

    for client in clients {
        client.join().expect("client thread");
    }

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_lists_modules_on_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs,Documentation"),
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

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), "docs\tDocumentation");

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line.trim_end(), "logs");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

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

#[test]
fn run_daemon_omits_unlisted_modules_from_listing() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[visible]\npath = /srv/visible\n\n[hidden]\npath = /srv/hidden\nlist = no\n",
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--bwlimit"),
            OsString::from("1K"),
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
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), "visible");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

