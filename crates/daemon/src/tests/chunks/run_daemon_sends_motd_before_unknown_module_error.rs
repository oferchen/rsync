#[test]
fn run_daemon_sends_motd_before_unknown_module_error() {
    // Wire/text fidelity: upstream emits the MOTD inside exchange_protocols()
    // (clientserver.c:158-170), immediately after the greeting and before it
    // reads the client's module request. The MOTD therefore precedes *every*
    // response, including an @ERROR refusal for an unknown module - not just
    // the module listing. A client parsing the daemon stream byte-for-byte
    // must see: greeting, MOTD body, blank separator, then @ERROR.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--motd-line"),
            OsString::from("Welcome to rsyncd"),
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

    stream
        .write_all(b"nosuchmod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // upstream: MOTD body precedes the refusal.
    line.clear();
    reader.read_line(&mut line).expect("motd line");
    assert_eq!(line, "Welcome to rsyncd\n");

    // upstream: clientserver.c:169 - single trailing blank line after the MOTD.
    line.clear();
    reader.read_line(&mut line).expect("motd trailing blank");
    assert_eq!(line, "\n");

    // upstream: clientserver.c:730 - "@ERROR: Unknown module '%s'\n"
    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert_eq!(line, "@ERROR: Unknown module 'nosuchmod'\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
