/// Regression test for upstream `daemon` testsuite: an empty module name
/// must be treated as a module-list request, matching upstream
/// `clientserver.c:1373` (`if (!*line || strcmp(line, "#list") == 0)`).
///
/// The runtests harness drives this path via `rsync host::` and the
/// `lsh.sh` stand-in: the client connects, performs the version exchange,
/// then sends an empty line. Before the fix, oc-rsync replied with
/// `@ERROR: daemon functionality is unavailable in this build`, causing
/// exit code 5 and breaking the upstream `daemon` test.
#[test]
fn daemon_negotiation_empty_module_lists_modules() {
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
            OsString::from(format!("docs={module_path},Documentation")),
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

    // Send only a bare newline. upstream: clientserver.c:1423 treats this
    // as equivalent to `#list`.
    stream.write_all(b"\n").expect("send empty request");
    stream.flush().expect("flush empty request");

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line, "docs           \tDocumentation\n");

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line, "logs           \t\n");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
