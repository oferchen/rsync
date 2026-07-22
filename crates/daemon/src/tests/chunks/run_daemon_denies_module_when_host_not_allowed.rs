#[test]
fn run_daemon_denies_module_when_host_not_allowed() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    // Use env::temp_dir() so the path exists on Windows (where /srv/docs
    // doesn't); the daemon refuses to start a module whose path doesn't
    // resolve and the test then panics with WSAECONNRESET on the greeting
    // read. Forward-slash-normalised to avoid the daemon module-arg parser's
    // backslash escape behaviour (see PR #4560 for the same root cause).
    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = {module_path}\nhosts allow = 10.0.0.0/8\n",
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

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    // upstream: clientserver.c:735 - access denied sends
    // "@ERROR: access denied to %s from %s (%s)\n" with (name, host, addr)
    // The host may be resolved to "localhost" or remain as "127.0.0.1"
    // depending on the system's DNS configuration.
    line.clear();
    reader.read_line(&mut line).expect("error message");
    let trimmed = line.trim_end();
    assert!(
        trimmed.starts_with("@ERROR: access denied to docs from ")
            && trimmed.ends_with("(127.0.0.1)"),
        "Expected upstream-format access denied message, got: {trimmed}"
    );

    // upstream: clientserver.c:381-385 - the client treats @ERROR as fatal and
    // returns before reading further, so the daemon sends no @RSYNCD: EXIT after
    // the refusal; the socket just closes (next read is EOF).
    line.clear();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(read, 0, "no trailing @RSYNCD: EXIT after @ERROR, got: {line:?}");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

