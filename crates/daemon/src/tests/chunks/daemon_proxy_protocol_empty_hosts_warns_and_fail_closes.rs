#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the trust gate is platform-independent and covered on Linux/macOS"
)]
fn daemon_proxy_protocol_empty_hosts_warns_and_fail_closes() {
    // upstream: clientserver.c:1747-1756 - `proxy protocol = true` with no
    // `proxy protocol hosts` is fail-closed BY DESIGN (access.c:302-303
    // rejects on an empty list), and because that silently drops every
    // connection the daemon warns the operator at startup, verbatim. The
    // 3.5.0 `proxy-protocol-trusted-peer` testsuite cell greps both lines.
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
            "proxy protocol = true\nlog file = {}\n\n[mod]\npath = {}\n",
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
    match reader.read_line(&mut line) {
        Ok(0) => {}
        Ok(_) => panic!("daemon answered despite trusting nobody: {line:?}"),
        Err(_) => {}
    }

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok());
    }

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    // upstream: clientserver.c:1752-1755 - the startup warning, verbatim
    // (note the double space before "Set").
    assert!(
        log_contents.contains(
            "\"proxy protocol = true\" but \"proxy protocol hosts\" is unset: \
             all connections will be rejected as untrusted proxy peers.  \
             Set \"proxy protocol hosts\" to your trusted proxy's address."
        ),
        "missing upstream-verbatim startup warning: {log_contents:?}"
    );
    // upstream: clientserver.c:1394 - and every peer is then rejected.
    assert!(
        log_contents
            .contains("proxy protocol rejected from untrusted peer UNDETERMINED (127.0.0.1)"),
        "empty trusted-proxy list must reject the peer: {log_contents:?}"
    );
}
