#[test]
fn run_daemon_enforces_bwlimit_during_module_list() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let comment = "x".repeat(4096);
    // Forward-slash-normalised env::temp_dir() so the daemon module-arg
    // parser doesn't swallow the comma separator via backslash escapes on
    // Windows (see PR #4560), and so the paths actually exist on Windows
    // where /srv/docs and /var/log don't (see PR #4559).
    let module_path = std::env::temp_dir()
        .display()
        .to_string()
        .replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--bwlimit"),
            OsString::from("1"),
            OsString::from("--module"),
            OsString::from(format!("docs={module_path},{comment}")),
            OsString::from("--module"),
            OsString::from(format!("logs={module_path}")),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    send_client_greeting(&mut stream);
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    // The daemon serves each session in a forked child on Unix (upstream:
    // socket.c:753-772 start_accept_loop), so the in-process sleep recorder
    // the bandwidth crate offers cannot observe the child's limiter. The
    // oracle is the wire itself: ~4.1 KB of module listing at `--bwlimit 1`
    // (1024 bytes/s; upstream io.c:962 writefd -> sleep_for_bwlimit paces
    // every daemon write) cannot complete in well under ~4 seconds, while an
    // unlimited daemon delivers it in milliseconds.
    let list_started = Instant::now();

    let mut total_bytes = 0usize;

    // upstream: no @RSYNCD: OK before module listing

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line, format!("docs           \t{comment}\n"));
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line, "logs           \t\n");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");
    total_bytes += line.len();

    let elapsed = list_started.elapsed();

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    // Full pacing of `total_bytes` at 1024 bytes/s is ~4s; require at least
    // half of it so a burst allowance cannot flake the assertion while an
    // unpaced listing (milliseconds) still fails it by orders of magnitude.
    // No upper bound: a loaded runner only ever makes the listing slower, and
    // a wedged daemon is bounded by the client stream's read timeout.
    let minimum = Duration::from_secs_f64(total_bytes as f64 / 1024.0 / 2.0);
    assert!(
        elapsed >= minimum,
        "module list of {total_bytes} bytes completed in {elapsed:?}; \
         a daemon honouring --bwlimit 1 needs at least {minimum:?}"
    );
}
