/// A refused option must reach the daemon log, in the words upstream uses.
///
/// upstream: `options.c:1409-1423` builds the refusal into `err_buf` once, and
/// `option_error()` (`options.c:907-918`) emits that same buffer via
/// `rprintf(FERROR, RSYNC_NAME ": %s", err_buf)`. A daemon's `FERROR` lands in
/// the log file, so the peer's `@ERROR` payload and the logged line are the
/// same string. Asserting both sides here is what pins them to one owner:
/// oc previously formatted them independently and they drifted, the log
/// carrying an oc-invented `refusing option '...' for module '...'` that no
/// upstream site emits.
#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the refusal path is platform-independent and covered on Linux/macOS"
)]
fn run_daemon_logs_a_refused_option_in_upstream_words() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let temp = tempdir().expect("log dir");
    let log_path = temp.path().join("rsyncd.log");

    let module_path = std::env::temp_dir()
        .display()
        .to_string()
        .replace('\\', "/");
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
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"@RSYNCD: OPTION --compress\n")
        .expect("send refused option");
    stream.flush().expect("flush refused option");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("refusal message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: The server is configured to refuse --compress",
    );

    drop(reader);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // upstream: options.c:915 - `rprintf(FERROR, RSYNC_NAME ": %s", err_buf)`.
    // The `rsync: ` prefix is `option_error()`'s; nothing else is added,
    // because the connection is already identified by the pid stamp and the
    // preceding `rsync allowed access on module ...` line.
    assert!(
        log_contents
            .lines()
            .any(|entry| entry.ends_with("rsync: The server is configured to refuse --compress")),
        "the log must carry upstream's refusal verbatim: {log_contents:?}"
    );
    // The peer and the log must not drift apart again: upstream has one
    // `err_buf`, so any wording the log invents for itself is a divergence.
    assert!(
        !log_contents.contains("refusing option"),
        "the log must not invent wording upstream never emits: {log_contents:?}"
    );
}
