use super::prelude::*;


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
fn run_module_list_reports_authentication_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abcdef";
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

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHFAILED credentials rejected\n")
                .expect("write failure");
            reader
                .get_mut()
                .write_all(b"@RSYNCD: EXIT\n")
                .expect("write exit");
            reader.get_mut().flush().expect("flush failure");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let error = run_module_list(request).expect_err("auth failure surfaces");
    set_test_daemon_password(None);

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("rejected provided credentials")
    );

    handle.join().expect("server thread");
}


#[test]
fn run_module_list_reports_access_denied() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: DENIED host rules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("denied response should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("denied access"));
    assert!(rendered.contains("host rules"));

    handle.join().expect("server thread");
}

