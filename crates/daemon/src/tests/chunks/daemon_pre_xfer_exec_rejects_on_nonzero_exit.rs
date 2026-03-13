#[cfg(unix)]
#[test]
fn daemon_pre_xfer_exec_rejects_on_nonzero_exit() {
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
            "[xfertest]\npath = {}\nread only = false\nuse chroot = false\npre-xfer exec = echo 'denied by hook' >&2; exit 1\n",
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
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    // Send client version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // Request the module
    stream
        .write_all(b"xfertest\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon sends OK for unauthenticated modules
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Send server args for a pull (--sender means server sends files)
    stream
        .write_all(b"--server\0--sender\0-logDtpr\0.\0xfertest/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    // Pre-xfer exec fails - daemon sends @ERROR with the stderr content
    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert!(
        line.starts_with("@ERROR:"),
        "expected @ERROR, got: {line}"
    );
    assert!(
        line.contains("denied by hook"),
        "expected stderr content in error, got: {line}"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
