#[test]
fn run_daemon_lists_modules_with_motd_lines() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(
        &motd_path,
        "Welcome to rsyncd\nRemember to sync responsibly\n",
    )
    .expect("write motd");

    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--motd-file"),
            motd_path.as_os_str().to_os_string(),
            OsString::from("--motd-line"),
            OsString::from("Additional notice"),
            OsString::from("--module"),
            OsString::from(format!("docs={module_path}")),
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

    // upstream: MOTD lines are sent as raw text, not wrapped in @RSYNCD: MOTD
    line.clear();
    reader.read_line(&mut line).expect("motd line 1");
    assert_eq!(line.trim_end(), "Welcome to rsyncd");

    line.clear();
    reader.read_line(&mut line).expect("motd line 2");
    assert_eq!(line.trim_end(), "Remember to sync responsibly");

    line.clear();
    reader.read_line(&mut line).expect("motd line 3");
    assert_eq!(line.trim_end(), "Additional notice");

    // upstream: clientserver.c:169 exchange_protocols() appends a single
    // unconditional `write_sbuf(f_out, "\n")` after the MOTD body, producing a
    // blank separator line before the module listing. A daemon that omits it
    // desynchronises byte-for-byte from upstream's greeting/MOTD framing.
    line.clear();
    reader.read_line(&mut line).expect("motd trailing blank");
    assert_eq!(line, "\n");

    // upstream: no @RSYNCD: OK before module listing

    line.clear();
    reader.read_line(&mut line).expect("module line");
    assert_eq!(line, "docs           \t\n");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

