#[test]
fn run_daemon_rejects_unknown_hash_command() {
    // upstream: clientserver.c:1427-1431 - a `#`-prefixed request that is not
    // `#list` is a command the daemon does not understand, rejected with
    // "@ERROR: Unknown command '%s'\n" (the raw line, leading `#` included).
    // This is distinct from the unknown-module response reserved for a bad
    // module name: it proves the daemon classifies `#bogus` as a command, not
    // as a module named "#bogus".
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
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"#bogus\n")
        .expect("send unknown command");
    stream.flush().expect("flush unknown command");

    // upstream: clientserver.c:1429 - "@ERROR: Unknown command '%s'\n" keeps
    // the leading `#`; it is NOT the "Unknown module" response.
    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert_eq!(line, "@ERROR: Unknown command '#bogus'\n");

    // upstream: the client treats @ERROR as fatal; the daemon closes the
    // socket without emitting @RSYNCD: EXIT, so the stream reaches EOF next.
    line.clear();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(read, 0);
    assert!(line.is_empty());

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
