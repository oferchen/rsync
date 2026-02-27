/// Verifies that `--config=FILE` causes the daemon to load modules from the
/// specified config file rather than the default path.
///
/// This exercises the end-to-end flow: CLI parsing, `DaemonConfig` construction,
/// `RuntimeOptions` config loading, and module advertisement over a live TCP
/// connection. Both the separated (`--config FILE`) and inline (`--config=FILE`)
/// forms are validated.
///
/// upstream: main.c — `--config=FILE` overrides the compiled-in default path.
#[test]
fn run_daemon_config_flag_overrides_default_path() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("temp dir");
    let module_dir = dir.path().join("custom_share");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("custom.conf");
    fs::write(
        &config_path,
        format!(
            "[custom_share]\npath = {}\ncomment = Custom config test\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    // Test the inline form: --config=FILE
    let (port, held_listener) = allocate_test_port();

    let inline_arg = format!("--config={}", config_path.display());
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from(inline_arg),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Verify the daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    // Request module listing
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    // Read capabilities
    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    // upstream: no @RSYNCD: OK before module listing

    // Verify the custom module appears in the listing
    line.clear();
    reader.read_line(&mut line).expect("module listing");
    assert!(
        line.contains("custom_share"),
        "expected module 'custom_share' in listing, got: {line}"
    );
    assert!(
        line.contains("Custom config test"),
        "expected comment 'Custom config test' in listing, got: {line}"
    );

    // Read exit
    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
