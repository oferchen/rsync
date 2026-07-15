// DMC-6: end-to-end test that proves a per-module `max connections = N`
// directive binds before the daemon-global `--max-connections` cap when
// the per-module value is smaller.
//
// Configuration: global cap = 10, per-module cap = 1. Two clients open
// connections to the same module; the first acquires the only slot the
// per-module cap allows, the second is refused with the upstream
// literal even though the daemon-global cap (10) is nowhere near hit.
//
// upstream: target/interop/upstream-src/rsync-3.4.1/clientserver.c:744
// applies `lp_max_connections(i)` via `claim_connection()` per module;
// the per-module value binds independently of any daemon-wide limit.

#[test]
fn run_daemon_per_module_cap_overrides_global_max_connections() {
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

    let (port, held_listener) = allocate_test_port();

    // The global cap is set to 10 - well above the per-module cap of 1.
    // Without per-module enforcement, both clients would slip past the
    // global gate. With per-module enforcement, the second client is
    // refused as soon as it names the module.
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
            OsString::from("--max-connections"),
            OsString::from("10"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let (mut first_stream, handle) = start_daemon(config, port, held_listener);
    let mut first_reader = BufReader::new(first_stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    first_reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    first_stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake");
    first_stream.flush().expect("flush handshake");

    first_stream
        .write_all(b"secure\n")
        .expect("send module request");
    first_stream.flush().expect("flush module");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("auth request for first client");
    assert!(line.starts_with("@RSYNCD: AUTHREQD"));

    // Second client targets the same module while the first still
    // holds its slot. The per-module cap (1) must fire even though the
    // global cap (10) has only one outstanding connection.
    let mut second_stream = connect_with_retries(port);
    let mut second_reader = BufReader::new(second_stream.try_clone().expect("clone second"));

    line.clear();
    second_reader.read_line(&mut line).expect("second greeting");
    assert_eq!(line, expected_greeting);

    second_stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send second handshake");
    second_stream.flush().expect("flush second handshake");

    second_stream
        .write_all(b"secure\n")
        .expect("send second module");
    second_stream.flush().expect("flush second module");

    // Pin the refusal line byte-for-byte to upstream rsync 3.4.1
    // `clientserver.c:752`. The cap value reported is the per-module
    // limit (1), not the global cap (10) - confirming the per-module
    // directive is the binding constraint.
    line.clear();
    second_reader.read_line(&mut line).expect("limit error");
    assert_eq!(
        line.trim_end(),
        "@ERROR: max connections (1) reached -- try again later",
    );

    // upstream: clientserver.c:381-385 - the client treats @ERROR as fatal and
    // returns before reading further, so no @RSYNCD: EXIT follows the refusal;
    // the socket just closes (next read is EOF).
    line.clear();
    let read = second_reader
        .read_line(&mut line)
        .expect("eof after error");
    assert_eq!(read, 0, "no trailing @RSYNCD: EXIT after @ERROR, got: {line:?}");

    first_stream
        .write_all(b"\n")
        .expect("send empty credentials to first client");
    first_stream.flush().expect("flush first credentials");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first denial message");
    assert!(line.starts_with("@ERROR: auth failed on module"));

    // upstream: clientserver.c:381-385 - the client treats @ERROR as fatal and
    // returns before reading further, so no @RSYNCD: EXIT follows the refusal;
    // the socket just closes (next read is EOF).
    line.clear();
    let read = first_reader
        .read_line(&mut line)
        .expect("eof after error");
    assert_eq!(read, 0, "no trailing @RSYNCD: EXIT after @ERROR, got: {line:?}");

    drop(second_reader);
    drop(second_stream);
    drop(first_reader);
    drop(first_stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
