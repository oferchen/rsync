#[test]
fn runtime_options_inherits_global_secrets_file_from_config() {
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
            "secrets file = {}\n[secure]\npath = {}\nauth users = alice\n",
            secrets_path.display(),
            module_dir.display()
        ),
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.auth_users(), &[String::from("alice")]);
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_inline_module_uses_global_secrets_file() {
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
        format!("secrets file = {}\n", secrets_path.display()),
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from(format!(
            "secure={}{}auth users=alice",
            module_dir.display(),
            ';'
        )),
    ];

    let options = RuntimeOptions::parse(&args).expect("parse inline module");
    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "secure");
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_inline_module_uses_default_secrets_file() {
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

    let args = [
        OsString::from("--module"),
        OsString::from(format!(
            "secure={}{}auth users=alice",
            module_dir.display(),
            ';'
        )),
    ];

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&args))
            .expect("parse inline module with default secrets");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "secure");
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_require_secrets_file_with_auth_users() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[secure]\npath = /srv/secure\nauth users = alice\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing the required 'secrets file' directive")
    );
}

#[cfg(unix)]
#[test]
fn runtime_options_rejects_world_readable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o644)).expect("chmod secrets");

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("world-readable secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("must not be accessible to group or others")
    );
}

#[test]
fn runtime_options_rejects_config_missing_path() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\ncomment = sample\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing path should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing required 'path' directive")
    );
}

#[test]
fn runtime_options_rejects_duplicate_module_across_config_and_cli() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("docs=/other/path"),
    ])
    .expect_err("duplicate module should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate module definition 'docs'")
    );
}

#[test]
fn run_daemon_serves_single_legacy_connection() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    line.clear();
    reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    stream.write_all(b"module\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert!(line.starts_with("@ERROR:"));

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_handles_binary_negotiation() {
    use rsync_protocol::{BorrowedMessageFrames, MessageCode};

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream
        .write_all(&advertisement)
        .expect("send client advertisement");
    stream.flush().expect("flush advertisement");

    let mut response = [0u8; 4];
    stream
        .read_exact(&mut response)
        .expect("read server advertisement");
    assert_eq!(response, advertisement);

    let mut frames = Vec::new();
    stream.read_to_end(&mut frames).expect("read frames");

    let mut iter = BorrowedMessageFrames::new(&frames);
    let first = iter.next().expect("first frame").expect("decode frame");
    assert_eq!(first.code(), MessageCode::Error);
    assert_eq!(first.payload(), HANDSHAKE_ERROR_PAYLOAD.as_bytes());
    let second = iter.next().expect("second frame").expect("decode frame");
    assert_eq!(second.code(), MessageCode::ErrorExit);
    assert_eq!(
        second.payload(),
        u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE)
            .expect("feature unavailable exit code fits")
            .to_be_bytes()
    );
    assert!(iter.next().is_none());

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_requests_authentication_for_protected_module() {
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

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    line.clear();
    reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    stream.write_all(b"secure\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(line.starts_with("@RSYNCD: AUTHREQD "));
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");
    assert!(!challenge.is_empty());

    stream.write_all(b"\n").expect("send empty credentials");
    stream.flush().expect("flush empty credentials");

    line.clear();
    reader.read_line(&mut line).expect("denied message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: access denied to module 'secure' from 127.0.0.1"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_enforces_module_connection_limit() {
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
    writeln!(
        fs::File::create(&config_path).expect("create config"),
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\nmax connections = 1\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let port = allocate_test_port();

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut first_stream = connect_with_retries(port);
    let mut first_reader = BufReader::new(first_stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    first_reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    first_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake");
    first_stream.flush().expect("flush handshake");

    line.clear();
    first_reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    first_stream
        .write_all(b"secure\n")
        .expect("send module request");
    first_stream.flush().expect("flush module");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("capabilities for first client");
    assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("auth request for first client");
    assert!(line.starts_with("@RSYNCD: AUTHREQD"));

    let mut second_stream = connect_with_retries(port);
    let mut second_reader = BufReader::new(second_stream.try_clone().expect("clone second"));

    line.clear();
    second_reader.read_line(&mut line).expect("second greeting");
    assert_eq!(line, expected_greeting);

    second_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send second handshake");
    second_stream.flush().expect("flush second handshake");

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    second_stream
        .write_all(b"secure\n")
        .expect("send second module");
    second_stream.flush().expect("flush second module");

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second capabilities");
    assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

    line.clear();
    second_reader.read_line(&mut line).expect("limit error");
    assert_eq!(
        line.trim_end(),
        "@ERROR: max connections (1) reached -- try again later"
    );

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    first_stream
        .write_all(b"\n")
        .expect("send empty credentials to first client");
    first_stream.flush().expect("flush first credentials");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first denial message");
    assert!(line.starts_with("@ERROR: access denied"));

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(second_reader);
    drop(second_stream);
    drop(first_reader);
    drop(first_stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

