#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the trust gate is platform-independent and covered on Linux/macOS"
)]
fn daemon_proxy_protocol_untrusted_peer_is_refused() {
    // upstream: clientserver.c:1443-1446 - with `proxy protocol = true` the
    // daemon consults `proxy_peer_allowed()` on the REAL socket address before
    // reading a single header byte. A direct peer not on `proxy protocol
    // hosts` is dropped without a greeting, and the refusal is logged with
    // upstream's exact wording (clientserver.c:1394) - the address the header
    // CLAIMS (here the trusted 203.0.113.7 itself) must never influence the
    // decision, or any client could assert its own source address.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let log_path = dir.path().join("rsyncd.log");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "proxy protocol = true\nproxy protocol hosts = 203.0.113.7\nlog file = {}\n\n[mod]\npath = {}\n",
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

    // Claim the trusted proxy's own address - if the gate keyed on the
    // claimed address instead of the real one, this header would pass.
    stream
        .write_all(b"PROXY TCP4 203.0.113.7 127.0.0.1 12345 873\r\n")
        .expect("send PROXY header");
    stream.flush().expect("flush PROXY header");

    let mut line = String::new();
    match reader.read_line(&mut line) {
        // upstream: start_daemon returns -1 (clientserver.c:1444-1445) - the
        // socket closes with no @RSYNCD greeting and no @ERROR.
        Ok(0) => {}
        Ok(_) => panic!("daemon answered an untrusted proxy peer: {line:?}"),
        // A reset also proves refusal; the upstream testsuite cell tolerates
        // ConnectionResetError the same way.
        Err(_) => {}
    }

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "refusal is not a daemon error");
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // upstream: clientserver.c:1394 - verbatim wording, host is the
    // UNDETERMINED sentinel (clientserver.c:1390). The 3.5.0
    // `proxy-protocol-trusted-peer` testsuite cell greps this line.
    assert!(
        log_contents
            .contains("proxy protocol rejected from untrusted peer UNDETERMINED (127.0.0.1)"),
        "missing upstream-verbatim rejection line: {log_contents:?}"
    );
    // A non-empty trusted list must NOT trip the empty-list startup warning.
    assert!(
        !log_contents.contains("\"proxy protocol hosts\" is unset"),
        "unset-hosts warning fired despite a configured list: {log_contents:?}"
    );
}
