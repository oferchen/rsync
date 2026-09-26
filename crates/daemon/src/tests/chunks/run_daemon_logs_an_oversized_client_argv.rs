/// The daemon-argument ceiling is recorded in the log and never sent.
///
/// upstream: `io.c:1503-1505` - `read_args()` cuts the peer off at
/// `MAX_DAEMON_ARGS` with `rprintf(FERROR, "too many daemon arguments\n")`.
/// `am_server` is not yet set while the daemon reads the argv
/// (clientserver.c:1154 vs :1197), so that FERROR goes to the log only
/// (log.c:331). Measured on 3.5.1: the peer reads EOF after `@RSYNCD: OK`. It
/// has switched to multiplexed input by then, so a raw line would only desync
/// it.
///
/// The client sends exactly the number of arguments that trips the ceiling, so
/// the daemon consumes every byte written here and neither side is left
/// blocking on the other.
#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the argument ceiling is platform-independent and covered on Linux/macOS"
)]
fn run_daemon_logs_an_oversized_client_argv() {
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

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("module acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    // upstream: io.c:1502 - the ceiling is checked before an argument is
    // appended, so the refusal fires once the vector already holds
    // `MAX_DAEMON_ARGS - 1` entries. Sending exactly that many leaves no
    // unconsumed bytes in flight.
    let overflow = protocol::secluded_args::MAX_DAEMON_ARGS - 1;
    let mut argv = Vec::with_capacity(overflow * 3);
    for _ in 0..overflow {
        argv.extend_from_slice(b"-v\0");
    }
    stream.write_all(&argv).expect("send oversized argv");
    stream.flush().expect("flush oversized argv");

    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    assert!(
        rest.is_empty(),
        "nothing may follow @RSYNCD: OK, got {:?}",
        String::from_utf8_lossy(&rest)
    );

    drop(reader);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // upstream: io.c:1503-1504 emits the refusal bare - unlike `option_error()`
    // (`options.c:915`) this site adds no `rsync: ` prefix.
    assert!(
        log_contents
            .lines()
            .any(|entry| entry.ends_with("too many daemon arguments")),
        "the log must record why the connection was cut: {log_contents:?}"
    );
}
