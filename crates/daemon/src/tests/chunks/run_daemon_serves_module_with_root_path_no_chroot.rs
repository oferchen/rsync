/// Daemon must accept and serve a module configured with `path = /` and
/// `use chroot = no` end-to-end (UTS-12 daemon-path-root-read scenario).
///
/// Upstream `loadparm.c` (P_PATH) preserves the bare slash and
/// `clientserver.c` serves it directly when `use chroot = no`. The unit
/// tests in `module_definition` and `config_parsing` cover the validator
/// gate; this exercise wires the runtime path: the daemon must start with
/// the config, advertise the module on `#list`, and emit `@RSYNCD: EXIT`
/// without falling over inside the connection handler.
#[test]
#[cfg(unix)]
fn run_daemon_serves_module_with_root_path_no_chroot() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("temp dir");
    let config_path = dir.path().join("root.conf");
    fs::write(
        &config_path,
        "[root]\npath = /\nuse chroot = no\nread only = yes\n",
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let inline_config = format!("--config={}", config_path.display());
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from(inline_config),
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

    line.clear();
    reader.read_line(&mut line).expect("module listing");
    assert_eq!(line, "root           \t\n");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
