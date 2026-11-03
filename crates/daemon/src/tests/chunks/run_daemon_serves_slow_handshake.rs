/// Regression: a client that is slow to complete the `@RSYNCD:` handshake must
/// still be served, not torn down. The daemon once armed a hardcoded 10-second
/// accept-time timeout, so a peer that paused longer than that between the
/// greeting and its request had the connection reset (soak run: 14/3000 pulls
/// under a CPU-starved 64-way burst). The only bound upstream puts on this
/// phase is the 60-second handshake deadline (clientserver.c:90, io.c:151-179).
///
/// The pause is modelled on the deadline's test clock rather than slept
/// through: the daemon observes 11 seconds as already spent, past the former
/// guard, and must still deliver the module listing.
///
/// `--no-detach` keeps the daemon in-thread so the assertions run in this
/// process; without it the daemon forks via `become_daemon()` and the parent
/// exits before the test body executes.
#[test]
fn run_daemon_serves_slow_handshake() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("temp dir");
    let module_dir = dir.path().join("archive");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("inline.conf");
    fs::write(
        &config_path,
        format!("[archive]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let inline_config = format!("--config={}", config_path.display());
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--no-detach"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from(inline_config),
            OsString::from("--once"),
        ])
        .build();

    // Pause past the former 10-second accept-time timeout before completing the
    // handshake. The daemon must wait rather than abort the connection. The
    // daemon is forked, so the offset is set before it starts for its copy of
    // the clock to carry it.
    let _paused = crate::daemon::handshake_deadline::test_clock::Advanced::by(
        std::time::Duration::from_secs(11),
    );
    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);


    // Complete the handshake before requesting, so this exercises the timeout
    // (what the test is for) rather than the startup-error path.
    send_client_greeting(&mut stream);
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("module listing after slow handshake");
    assert_eq!(
        line, "archive        \t\n",
        "slow handshake must still receive the module listing, not a reset"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
