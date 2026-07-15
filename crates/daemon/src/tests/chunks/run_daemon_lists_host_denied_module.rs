#[test]
fn run_daemon_lists_host_denied_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    // env::temp_dir() so the path exists on Windows; forward-slash normalised
    // so the daemon module-arg parser doesn't swallow escapes (see PR #4560).
    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    // `private` restricts access to 10.0.0.0/8, so the loopback client is
    // denied on connect. It is still `list = yes` (the default), so it MUST
    // appear in the listing. Visibility and access are separate concerns.
    writeln!(
        file,
        "[public]\npath = {module_path}\n\n[private]\npath = {module_path}\nhosts allow = 10.0.0.0/8\n",
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    // upstream: clientserver.c:1261-1272 send_listing() lists every module with
    // `list = yes` and applies NO hosts allow/deny filtering. The host-denied
    // `private` module therefore appears alongside `public`; the deny only
    // takes effect on the module-open path (clientserver.c:728 allow_access).
    line.clear();
    reader.read_line(&mut line).expect("public module");
    assert_eq!(line, "public         \t\n");

    line.clear();
    reader.read_line(&mut line).expect("host-denied private module");
    assert_eq!(line, "private        \t\n");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("exit line after all listable modules");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
