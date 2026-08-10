/// NXT-8 regression test ported from the upstream `daemon` testsuite: a
/// module-list request must complete - returning the listing and
/// `@RSYNCD: EXIT` - even when the listed module carries a `timeout`
/// directive.
///
/// # Why this matters
///
/// Upstream applies a module's timeout only *after* a module has been
/// selected (`clientserver.c:1192` sets the io timeout against the chosen
/// `module_id`). The module listing (`clientserver.c:1373`,
/// `send_listing()`) runs *before* any module is selected, so a per-module
/// timeout must neither gate nor block the pre-selection listing path. A
/// naive implementation that armed the module read/write timeout before
/// emitting the listing could stall or truncate `localhost::` listings; this
/// test pins that the listing path stays independent of module timeouts.
#[test]
fn run_daemon_lists_modules_with_module_timeout() {
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
            // A finite per-module timeout that must not interfere with the
            // pre-selection listing path.
            OsString::from("--module"),
            OsString::from(format!("docs={module_path},Documentation;timeout=1")),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    // upstream: clientserver.c:1373 - an empty request lists modules.
    stream.write_all(b"\n").expect("send empty request");
    stream.flush().expect("flush empty request");

    line.clear();
    reader.read_line(&mut line).expect("module line");
    assert_eq!(line, "docs           \tDocumentation\n");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
