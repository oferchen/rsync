/// upstream: authenticate.c:411-417 - a `:deny` rule refuses with "denied by
/// rule" before check_secret() is called. The wrong password here would make
/// a password-first daemon log a credential failure instead, so the log line
/// shows which check actually decided the refusal.
#[test]
fn run_daemon_deny_rule_refuses_before_checking_the_password() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:correctpassword\n").expect("write secrets");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }
    let log_path = dir.path().join("rsyncd.log");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "log file = {}\n[protected]\npath = {}\nauth users = alice:deny\nsecrets file = {}\nuse chroot = false\n",
            log_path.display(),
            module_dir.display(),
            secrets_path.display()
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
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream
        .write_all(b"protected\n")
        .expect("send module request");
    stream.flush().expect("flush module request");
    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(
        line.starts_with("@RSYNCD: AUTHREQD "),
        "expected auth challenge, got: {line}"
    );
    stream
        .write_all(b"alice d3JvbmdwYXNzd29yZGRpZ2VzdA\n")
        .expect("send wrong credentials");
    stream.flush().expect("flush wrong credentials");
    line.clear();
    reader.read_line(&mut line).expect("denied message");
    assert_eq!(line.trim_end(), "@ERROR: auth failed on module protected");
    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    let log = fs::read_to_string(&log_path).expect("read daemon log");
    assert!(
        log.contains("auth failed on module protected")
            && log.contains("for alice: denied by rule"),
        "the :deny rule, not the password, must decide the refusal; log:\n{log}"
    );
}
