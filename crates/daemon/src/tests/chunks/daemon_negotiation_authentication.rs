/// Tests for daemon mode authentication negotiation.
///
/// These tests verify the correct behavior of the daemon's authentication
/// flow during the negotiation protocol, including challenge-response auth.

#[test]
fn daemon_negotiation_auth_challenge_is_unique_per_session() {
    // Verify that authentication challenges differ between sessions.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

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
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    // We'll connect twice and collect challenges
    let mut challenges = Vec::new();

    for _ in 0..2 {
        let port = allocate_test_port();

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

        let handle = thread::spawn(move || run_daemon(config));

        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        // Read greeting
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");

        // Send version response
        stream
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send version");
        stream.flush().expect("flush");

        // Request the secure module
        stream.write_all(b"secure\n").expect("send module");
        stream.flush().expect("flush");

        // Read AUTHREQD challenge
        line.clear();
        reader.read_line(&mut line).expect("auth challenge");
        let challenge = line
            .trim_end()
            .strip_prefix("@RSYNCD: AUTHREQD ")
            .expect("challenge prefix")
            .to_string();
        challenges.push(challenge);

        // Close connection
        drop(reader);
        drop(stream);

        // Small sleep to allow daemon to fully process
        thread::sleep(Duration::from_millis(50));
        let _result = handle.join();
    }

    // Challenges should be different
    assert_eq!(challenges.len(), 2);
    assert_ne!(
        challenges[0], challenges[1],
        "Challenges should be unique per session"
    );
}

#[test]
fn daemon_negotiation_auth_denies_wrong_password() {
    // Verify that incorrect passwords are rejected.
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
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version response
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request secure module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Read AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");

    // Compute digest with WRONG password
    let mut hasher = Md5::new();
    hasher.update(b"wrongpassword");
    hasher.update(challenge.as_bytes());
    let digest = STANDARD_NO_PAD.encode(hasher.finalize());
    let response = format!("alice {digest}\n");

    stream
        .write_all(response.as_bytes())
        .expect("send wrong credentials");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("access denied"),
        "Expected access denied, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_auth_denies_unknown_user() {
    // Verify that unknown usernames are rejected.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

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
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version response
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request secure module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Read AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");

    // Compute digest with unknown user
    let mut hasher = Md5::new();
    hasher.update(b"password");
    hasher.update(challenge.as_bytes());
    let digest = STANDARD_NO_PAD.encode(hasher.finalize());
    let response = format!("bob {digest}\n");

    stream
        .write_all(response.as_bytes())
        .expect("send unknown user");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("access denied"),
        "Expected access denied, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_auth_skipped_for_unprotected_module() {
    // Verify that modules without auth_users don't request authentication.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[public]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version response
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request public module
    stream.write_all(b"public\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive OK directly (no AUTHREQD)
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert_eq!(
        line, "@RSYNCD: OK\n",
        "Expected OK without auth for public module, got: {line}"
    );

    drop(reader);
    // Don't assert on result - daemon may fail gracefully when client doesn't send transfer data
    let _ = handle.join();
}

#[test]
fn daemon_negotiation_auth_denies_empty_credentials() {
    // Verify that empty username/password is rejected.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

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
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version response
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request secure module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Read AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    assert!(line.starts_with("@RSYNCD: AUTHREQD"));

    // Send empty credentials
    stream
        .write_all(b"\n")
        .expect("send empty credentials");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("access denied"),
        "Expected access denied for empty credentials, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_auth_successful_sends_ok() {
    // Verify that successful authentication sends OK.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:secretpass\n").expect("write secrets");

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
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version response
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush");

    // Request secure module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Read AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");

    // Compute correct digest
    let mut hasher = Md5::new();
    hasher.update(b"secretpass");
    hasher.update(challenge.as_bytes());
    let digest = STANDARD_NO_PAD.encode(hasher.finalize());
    let response = format!("alice {digest}\n");

    stream
        .write_all(response.as_bytes())
        .expect("send credentials");
    stream.flush().expect("flush");

    // Should receive OK
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert_eq!(line, "@RSYNCD: OK\n", "Expected OK after successful auth");

    drop(reader);
    // Don't assert on result - daemon may fail gracefully when client doesn't continue
    let _ = handle.join();
}
