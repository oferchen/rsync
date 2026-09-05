/// The daemon-argument ceiling must be recorded, not only sent to the peer.
///
/// upstream: `io.c:1476-1479` - `read_args()` cuts the peer off at
/// `MAX_DAEMON_ARGS` with `rprintf(FERROR, "too many daemon arguments\n")`, and
/// a daemon's `FERROR` reaches the log file. oc answered the peer and logged
/// nothing, so the guard fired invisibly: an operator saw a connection cut with
/// no recorded reason.
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

    // upstream: io.c:1476 - the ceiling is checked before an argument is
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

    line.clear();
    reader.read_line(&mut line).expect("argument refusal");
    assert!(
        line.contains("too many daemon arguments"),
        "the peer must be told upstream's reason: {line:?}"
    );

    drop(reader);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // upstream: io.c:1477-1478 emits the refusal bare - unlike `option_error()`
    // (`options.c:915`) this site adds no `rsync: ` prefix.
    assert!(
        log_contents
            .lines()
            .any(|entry| entry.ends_with("too many daemon arguments")),
        "the log must record why the connection was cut: {log_contents:?}"
    );
}
