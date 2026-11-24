/// An other-accessible secrets file under `strict modes` logs upstream's rule
/// line, then the auth failure naming "ignoring secrets file".
///
/// upstream: authenticate.c:162-177, :428.
#[cfg(unix)]
#[test]
fn other_accessible_secrets_file_logs_the_strict_modes_line() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets = dir.path().join("secrets");
    fs::write(&secrets, "alice:right\n").expect("write secrets");
    fs::set_permissions(&secrets, PermissionsExt::from_mode(0o644)).expect("chmod secrets");
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let log_path = dir.path().join("rsyncd.log");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "log file = {}\n[mod]\nuse chroot = false\npath = {}\nauth users = alice\nsecrets file = {}\n",
            log_path.display(),
            module_dir.display(),
            secrets.display()
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
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nmod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");
    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(line.starts_with("@RSYNCD: AUTHREQD "), "got: {line}");
    stream
        .write_all(b"alice d3JvbmdwYXNzd29yZGRpZ2VzdA\n")
        .expect("send credentials");
    stream.flush().expect("flush credentials");
    line.clear();
    reader.read_line(&mut line).expect("denied message");
    assert_eq!(line.trim_end(), "@ERROR: auth failed on module mod");
    drop(reader);
    drop(stream);
    let _ = handle.join().expect("daemon thread");
    let log = fs::read_to_string(&log_path).expect("read daemon log");
    let messages: Vec<&str> = log
        .lines()
        .map(|entry| entry.split_once("] ").map_or(entry, |(_, body)| body))
        .collect();
    let failure = messages
        .iter()
        .position(|message| message.starts_with("auth failed on module mod from "))
        .unwrap_or_else(|| panic!("no auth failure line in {messages:?}"));
    assert!(
        messages[failure].ends_with(" for alice: ignoring secrets file"),
        "got {:?}",
        messages[failure]
    );
    assert_eq!(
        messages[failure - 1],
        "secrets file must not be other-accessible (see strict modes option)"
    );
}
