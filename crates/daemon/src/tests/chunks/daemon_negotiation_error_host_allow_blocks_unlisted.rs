#[test]
fn daemon_negotiation_error_host_allow_blocks_unlisted() {
    // When hosts_allow is set to an address that does NOT match the loopback
    // test connection, the daemon must deny access with "@ERROR: access denied".
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[restricted]\npath = {}\nhosts allow = 203.0.113.0/24\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request module (our loopback IP is not in 203.0.113.0/24)
    stream.write_all(b"restricted\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("access denied"),
        "Expected access denied when hosts allow does not include loopback, got: {line}"
    );

    // upstream: clientserver.c:381-385 - the client treats @ERROR as fatal and
    // returns before reading further, so the daemon sends no @RSYNCD: EXIT after
    // the refusal; the socket just closes (next read is EOF).
    line.clear();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(read, 0, "no trailing @RSYNCD: EXIT after @ERROR, got: {line:?}");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
