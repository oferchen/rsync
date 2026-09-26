/// Sends `lines` after the version greeting to a `--once` daemon serving one
/// module `[docs]` and returns the first line the daemon answers with.
fn first_reply_after(lines: &[u8]) -> String {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[docs]\npath = {}\nuse chroot = false\n",
            dir.path().display()
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
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send greeting");
    let _ = stream.write_all(lines);
    let _ = stream.flush();

    line.clear();
    let _ = reader.read_line(&mut line);
    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    line
}

/// upstream: clientserver.c:1540-1543 - a length outside `1..=BIGPATHBUFLEN`
/// is refused with an @ERROR line, not a silent close. Measured on 3.5.1.
#[test]
fn run_daemon_refuses_an_invalid_early_input_length() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    for command in [
        &b"#early_input=0\n"[..],
        b"#early_input=-1\n",
        b"#early_input=9999\n",
    ] {
        assert_eq!(
            first_reply_after(command),
            "@ERROR: invalid early_input length\n",
            "{}",
            String::from_utf8_lossy(command)
        );
    }
}

/// upstream: clientserver.c:1548-1561 - after the early-input payload the
/// daemon reads exactly one request line, so a second `#early_input=` is an
/// unknown `#` command. Measured on 3.5.1.
#[test]
fn run_daemon_treats_a_second_early_input_as_an_unknown_command() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    assert_eq!(
        first_reply_after(b"#early_input=3\nabc#early_input=3\nxyzdocs\n"),
        "@ERROR: Unknown command '#early_input=3'\n"
    );
}
