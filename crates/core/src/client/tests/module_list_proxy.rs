#[test]
fn establish_proxy_tunnel_formats_ipv6_authority_without_brackets() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
    let addr = listener.local_addr().expect("proxy addr");
    let expected_line = "CONNECT fe80::1%eth0:873 HTTP/1.0\r\n";

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept proxy connection");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read CONNECT request");
        assert_eq!(line, expected_line);

        line.clear();
        reader.read_line(&mut line).expect("read blank line");
        assert!(line == "\r\n" || line == "\n");

        let mut stream = reader.into_inner();
        stream
            .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
            .expect("write proxy response");
        stream.flush().expect("flush proxy response");
    });

    let daemon_addr = DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("daemon addr");
    let proxy = ProxyConfig {
        host: String::from("proxy.example"),
        port: addr.port(),
        credentials: None,
    };

    let mut stream = TcpStream::connect(addr).expect("connect to proxy listener");
    establish_proxy_tunnel(&mut stream, &daemon_addr, &proxy).expect("tunnel negotiation succeeds");

    handle.join().expect("proxy thread completes");
}

#[test]
fn run_module_list_accepts_lowercase_proxy_status_line() {
    let responses = vec!["@RSYNCD: OK\n", "kappa\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, _request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, LOWERCASE_PROXY_STATUS_LINE);

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "kappa");

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_reports_invalid_proxy_configuration() {
    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_PROXY", "invalid-proxy");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(String::from("localhost"), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("invalid proxy should fail");
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY must be in HOST:PORT form")
    );
}

#[test]
fn parse_proxy_spec_accepts_http_scheme() {
    let proxy =
        parse_proxy_spec("http://user:secret@proxy.example:8080").expect("http proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 8080);
    assert_eq!(proxy.authorization_header(), Some("dXNlcjpzZWNyZXQ="));
}

#[test]
fn parse_proxy_spec_decodes_percent_encoded_credentials() {
    let proxy = parse_proxy_spec("http://user%3Aname:p%40ss%25word@proxy.example:1080")
        .expect("percent-encoded proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 1080);
    assert_eq!(
        proxy.authorization_header(),
        Some("dXNlcjpuYW1lOnBAc3Mld29yZA==")
    );
}

#[test]
fn parse_proxy_spec_caches_authorization_header() {
    let proxy = parse_proxy_spec("http://user:secret@proxy.example:1080").expect("proxy parses");
    let first = proxy.authorization_header().expect("header value");
    let second = proxy
        .authorization_header()
        .expect("header value reused");
    assert!(
        std::ptr::eq(first, second),
        "authorization header should reuse cached storage"
    );
}

#[test]
fn parse_proxy_spec_accepts_https_scheme() {
    let proxy = parse_proxy_spec("https://proxy.example:3128").expect("https proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 3128);
    assert!(proxy.authorization_header().is_none());
}

#[test]
fn parse_proxy_spec_rejects_unknown_scheme() {
    let error = match parse_proxy_spec("socks5://proxy:1080") {
        Ok(_) => panic!("invalid proxy scheme should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY scheme must be http:// or https://")
    );
}

#[test]
fn parse_proxy_spec_rejects_path_component() {
    let error = match parse_proxy_spec("http://proxy.example:3128/path") {
        Ok(_) => panic!("proxy specification with path should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY must not include a path component")
    );
}

#[test]
fn parse_proxy_spec_rejects_invalid_percent_encoding_in_credentials() {
    let error = match parse_proxy_spec("user%zz:secret@proxy.example:8080") {
        Ok(_) => panic!("invalid percent-encoding should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY username contains invalid percent-encoding")
    );

    let error = match parse_proxy_spec("user:secret%@proxy.example:8080") {
        Ok(_) => panic!("truncated percent-encoding should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY password contains truncated percent-encoding")
    );
}

#[test]
fn run_module_list_reports_daemon_error() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_daemon_error_without_colon() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@ERROR unavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn map_daemon_handshake_error_converts_error_payload() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@ERROR module unavailable".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("module unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_plain_invalid_data_error() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::InvalidData, "@ERROR daemon unavailable");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("daemon unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_other_malformed_greetings() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD? unexpected".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
    assert!(mapped.message().to_string().contains("@RSYNCD? unexpected"));
}

#[test]
fn map_daemon_handshake_error_propagates_other_failures() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::TimedOut, "timed out");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(rendered.contains("timed out"));
    assert!(rendered.contains("negotiate with"));
}

#[test]
fn run_module_list_reports_daemon_error_with_case_insensitive_prefix() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@error:\tunavailable\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon error should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(error.message().to_string().contains("unavailable"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_authentication_required() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: AUTHREQD modules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("auth requirement should surface");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("requires authentication"));
    assert!(rendered.contains("username"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_requires_password_for_authentication() {
    let responses = vec!["@RSYNCD: AUTHREQD challenge\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(None);

    let error = run_module_list(request).expect_err("missing password should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(error.message().to_string().contains("RSYNC_PASSWORD"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abc123";
    let expected = compute_daemon_auth_response(b"secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "secured\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "secured");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_password_override() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind override daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "override";
    let expected = compute_daemon_auth_response(b"override-secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "override\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"wrong".to_vec()));
    let list = run_module_list_with_password(
        request,
        Some(b"override-secret".to_vec()),
        TransferTimeout::Default,
    )
    .expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "override");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_split_challenge() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind split auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "split123";
    let expected = compute_daemon_auth_response(b"secret", challenge);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHREQD\n")
                .expect("write authreqd");
            reader.get_mut().flush().expect("flush authreqd");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTH {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "protected\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "protected");

    handle.join().expect("server thread");
}

