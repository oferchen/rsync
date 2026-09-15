#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the trust gate is platform-independent and covered on Linux/macOS"
)]
fn daemon_proxy_protocol_trusted_peer_header_is_honoured() {
    // upstream: clientserver.c:1443-1446 - a peer listed in `proxy protocol
    // hosts` clears `proxy_peer_allowed()`, its PROXY header is read, and the
    // CLAIMED address replaces the socket address for everything downstream:
    // `hosts allow`/`hosts deny`, `%a`, and the `connect from %s (%s)` log
    // line (clientserver.c:1526) all see the proxied client, not the proxy.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let log_path = dir.path().join("rsyncd.log");

    // `reverse lookup = false` keeps the test off DNS (10.9.8.7 has no PTR
    // record worth waiting for) and pins the UNDETERMINED sentinel in the
    // connect line, exactly as upstream renders it (clientserver.c:1525).
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "proxy protocol = true\nproxy protocol hosts = 127.0.0.1\nreverse lookup = false\nlog file = {}\n\n[mod]\npath = {}\n",
            log_path.display(),
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

    stream
        .write_all(b"PROXY TCP4 10.9.8.7 127.0.0.1 12345 873\r\n")
        .expect("send PROXY header");
    stream.flush().expect("flush PROXY header");

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "trusted proxy peer must be greeted, got: {line:?}"
    );

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send version");
    stream.write_all(b"#list\n").expect("request module list");
    stream.flush().expect("flush");

    let mut rest = String::new();
    reader.read_to_string(&mut rest).expect("module list");
    assert!(rest.contains("mod"), "module list expected, got: {rest:?}");

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // The claimed address, not the socket's 127.0.0.1, is the peer.
    assert!(
        log_contents.contains("connect from UNDETERMINED (10.9.8.7)"),
        "connect line must carry the PROXY-claimed address: {log_contents:?}"
    );
    assert!(
        !log_contents.contains("proxy protocol rejected"),
        "a listed trusted proxy must not be rejected: {log_contents:?}"
    );
}
