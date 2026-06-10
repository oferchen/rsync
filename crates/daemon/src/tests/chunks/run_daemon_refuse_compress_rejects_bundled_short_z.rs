#[test]
fn run_daemon_refuse_compress_rejects_bundled_short_z() {
    // upstream: testsuite/daemon-refuse-compress_test.py exercises the post-OK
    // client-args round-trip. A module with `refuse options = compress` must
    // reject `rsync -avz`, which is delivered to the daemon as a bundled
    // server argstr (e.g. `-vlogDtprez.iLsfxCIvu`) where the `z` letter sits
    // inside the packed short-option run.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = {module_path}\nrefuse options = compress\n",
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
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n", "Expected OK before client args");

    // Post-OK client argv mirrors upstream's `read_args()` payload for
    // `rsync -avz src/ user@host::docs/`: protocol 32 uses NUL-terminated
    // arguments and an empty arg (`\0`) marks end-of-list.
    // upstream: io.c:1292 - read_args() with use_nulls=1 for protocol >= 30.
    let args = b"--server\0-vlogDtprez.iLsfxCIvu\0.\0docs/\0\0";
    stream.write_all(args).expect("send client args");
    stream.flush().expect("flush client args");

    line.clear();
    reader.read_line(&mut line).expect("refusal message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: The server is configured to refuse --compress",
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
