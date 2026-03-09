#[test]
fn run_daemon_auth_failure_rejects_wrong_credentials() {
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

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[protected]\npath = {}\nauth users = alice\nsecrets file = {}\nuse chroot = false\n",
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

    // Read daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    // Send client version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // Request the authenticated module
    stream
        .write_all(b"protected\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon responds with AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(
        line.starts_with("@RSYNCD: AUTHREQD "),
        "expected auth challenge, got: {line}"
    );
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");
    assert!(!challenge.is_empty());

    // Send wrong credentials - compute MD5 digest with wrong password.
    // upstream: authenticate() in authenticate.c — client sends
    // "username base64(MD5(password + challenge))\n"
    let wrong_password = "wrongpassword";
    let mut digest_input = Vec::new();
    digest_input.extend_from_slice(b"\0\0\0\0");
    digest_input.extend_from_slice(wrong_password.as_bytes());
    digest_input.extend_from_slice(challenge.as_bytes());
    let md5_hash = Md5::digest(&digest_input);
    let encoded = STANDARD_NO_PAD.encode(md5_hash);
    let auth_response = format!("alice {encoded}\n");

    stream
        .write_all(auth_response.as_bytes())
        .expect("send wrong credentials");
    stream.flush().expect("flush wrong credentials");

    // upstream: clientserver.c:762 — auth failure sends
    // "@ERROR: auth failed on module %s\n"
    line.clear();
    reader.read_line(&mut line).expect("denied message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: auth failed on module protected"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
