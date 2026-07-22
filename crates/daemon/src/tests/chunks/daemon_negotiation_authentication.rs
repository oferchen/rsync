/// Tests for daemon mode authentication negotiation.
///
/// These tests verify the correct behavior of the daemon's authentication
/// flow during the negotiation protocol, including challenge-response auth.

#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; negotiation is platform-independent and covered on Linux/macOS"
)]
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

    // Use a single daemon with `--max-sessions 2` instead of spawning two
    // sequential `--once` daemons. Two back-to-back daemon start/teardown
    // cycles on the same test thread race the listener shutdown on Windows,
    // and the second connection observes WSAECONNRESET (10054) before the
    // worker writes the greeting. The single-daemon pattern is what
    // `run_daemon_honours_max_sessions` already uses successfully on Windows.
    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    // The daemon needs to bind itself to handle two sequential connections;
    // release the test's holding listener so the daemon's bind to the same
    // port succeeds (the held listener was only needed to reserve the port).
    drop(held_listener);
    let handle = thread::spawn(move || run_daemon(config));

    let mut challenges = Vec::new();
    for _ in 0..2 {
        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");

        stream
            .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
            .expect("send version");
        stream.flush().expect("flush");

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

        // Close the per-session connection so the daemon's worker finishes
        // and the accept loop is free to service the next iteration.
        drop(reader);
        drop(stream);
    }

    // The daemon exits after serving both sessions (max-sessions = 2). On
    // Windows CI a blocking accept on the re-bound listener can linger past the
    // client disconnect, so bound the join and detach if it does not finish
    // rather than wedging the test until nextest's 360s slow-timeout fires.
    let _ = finish_daemon(handle);

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
        .expect("send version");
    stream.flush().expect("flush");

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

    // upstream: clientserver.c:764 - auth failure sends
    // "@ERROR: auth failed on module %s\n"
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("auth failed on module"),
        "Expected auth failed, got: {line}"
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
        .expect("send version");
    stream.flush().expect("flush");

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

    // upstream: clientserver.c:764 - auth failure sends
    // "@ERROR: auth failed on module %s\n"
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("auth failed on module"),
        "Expected auth failed, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; negotiation is platform-independent and covered on Linux/macOS"
)]
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
        .expect("send version");
    stream.flush().expect("flush");

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
    // Don't assert on result - daemon may fail gracefully when client doesn't
    // send transfer data. Bound the join: on Windows the daemon's accept loop
    // can linger past the client disconnect, so detach rather than wedge until
    // nextest's 360s slow-timeout fires.
    let _ = finish_daemon(handle);
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
        .expect("send version");
    stream.flush().expect("flush");

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

    // upstream: clientserver.c:764 - auth failure sends
    // "@ERROR: auth failed on module %s\n"
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") && line.contains("auth failed on module"),
        "Expected auth failed for empty credentials, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; negotiation is platform-independent and covered on Linux/macOS"
)]
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
        .expect("send version");
    stream.flush().expect("flush");

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
    // Don't assert on result - daemon may fail gracefully when client doesn't
    // continue. Bound the join: on Windows the accept loop can linger past the
    // disconnect, so detach rather than wedge until nextest's 360s slow-timeout.
    let _ = finish_daemon(handle);
}
