#[test]
fn module_list_request_decodes_percent_encoded_host() {
    let operands = vec![OsString::from("rsync://example%2Ecom/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_raw_ipv6_zone_identifier() {
    let operands = vec![OsString::from("[fe80::1%eth0]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_decodes_percent_encoded_username() {
    let operands = vec![OsString::from("user%2Bname@localhost::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.username(), Some("user+name"));
    assert_eq!(request.address().host(), "localhost");
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding_in_username() {
    let operands = vec![OsString::from("user%2@localhost::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("invalid encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon username")
    );
}

#[test]
fn module_list_request_defaults_to_localhost_for_shorthand() {
    let operands = vec![OsString::from("::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert!(request.username().is_none());
}

#[test]
fn module_list_request_preserves_username_with_default_host() {
    let operands = vec![OsString::from("user@::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}

#[test]
fn module_list_options_reports_address_mode() {
    let options = ModuleListOptions::default().with_address_mode(AddressMode::Ipv6);
    assert_eq!(options.address_mode(), AddressMode::Ipv6);

    let default_options = ModuleListOptions::default();
    assert_eq!(default_options.address_mode(), AddressMode::Default);
}

#[test]
fn module_list_options_records_bind_address() {
    let socket = "198.51.100.4:0".parse().expect("socket");
    let options = ModuleListOptions::default().with_bind_address(Some(socket));
    assert_eq!(options.bind_address(), Some(socket));

    let default_options = ModuleListOptions::default();
    assert!(default_options.bind_address().is_none());
}

#[test]
fn module_list_options_retains_sockopts() {
    let options = ModuleListOptions::default()
        .with_sockopts(Some(OsString::from("SO_SNDBUF=16384")));
    assert_eq!(
        options.sockopts(),
        Some(std::ffi::OsStr::new("SO_SNDBUF=16384"))
    );

    let default_options = ModuleListOptions::default();
    assert!(default_options.sockopts().is_none());
}

#[test]
fn module_list_options_tracks_blocking_preference() {
    let options = ModuleListOptions::default().with_blocking_io(Some(false));
    assert_eq!(options.blocking_io(), Some(false));

    let default_options = ModuleListOptions::default();
    assert!(default_options.blocking_io().is_none());
}

#[test]
fn resolve_daemon_addresses_filters_ipv4_mode() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv4).expect("ipv4 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv4));
}

#[test]
fn resolve_daemon_addresses_rejects_missing_ipv6_addresses() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let error = resolve_daemon_addresses(&address, AddressMode::Ipv6)
        .expect_err("IPv6 filtering should fail for IPv4-only host");

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("does not have IPv6 addresses"));
}

#[test]
fn resolve_daemon_addresses_filters_ipv6_mode() {
    let address = DaemonAddress::new(String::from("::1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv6).expect("ipv6 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv6));
}

#[test]
fn daemon_address_accepts_ipv6_zone_identifier() {
    let address =
        DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("zone identifier accepted");
    assert_eq!(address.host(), "fe80::1%eth0");
    assert_eq!(address.port(), 873);

    let display = format!("{}", address.socket_addr_display());
    assert_eq!(display, "[fe80::1%eth0]:873");
}

#[test]
fn module_list_request_parses_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://fe80::1%eth0/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);

    let bracketed = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&bracketed)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding() {
    let operands = vec![OsString::from("rsync://example%2/")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("truncated percent encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon host")
    );
}

#[test]
fn daemon_address_trims_host_whitespace() {
    let address =
        DaemonAddress::new("  example.com  ".to_string(), 873).expect("address trims host");
    assert_eq!(address.host(), "example.com");
    assert_eq!(address.port(), 873);
}

#[test]
fn module_list_request_rejects_empty_username() {
    let operands = vec![OsString::from("@example.com::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("empty username should be rejected");
    let rendered = error.message().to_string();
    assert!(rendered.contains("daemon username must be non-empty"));
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
}

#[test]
fn module_list_request_rejects_ipv6_module_transfer() {
    let operands = vec![OsString::from("[fe80::1]::module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}

#[test]
fn module_list_request_requires_bracketed_ipv6_host() {
    let operands = vec![OsString::from("fe80::1::")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("unbracketed IPv6 host should be rejected");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("IPv6 daemon addresses must be enclosed in brackets")
    );
}

#[test]
fn run_module_list_collects_entries() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: MOTD Maintenance window at 02:00 UTC\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "beta\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[
            String::from("Welcome to the test daemon"),
            String::from("Maintenance window at 02:00 UTC"),
        ]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 2);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));
    assert_eq!(list.entries()[1].name(), "beta");
    assert_eq!(list.entries()[1].comment(), None);

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_uses_connect_program_command() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let command = OsString::from(
        "sh -c 'CONNECT_HOST=%H\n\
         CONNECT_PORT=%P\n\
         printf \"@RSYNCD: 31.0\\n\"\n\
         read greeting\n\
         printf \"@RSYNCD: OK\\n\"\n\
         read request\n\
         printf \"example\\t$CONNECT_HOST:$CONNECT_PORT\\n@RSYNCD: EXIT\\n\"'",
    );

    let _prog_guard = EnvGuard::set_os("RSYNC_CONNECT_PROG", &command);
    let _shell_guard = EnvGuard::remove("RSYNC_SHELL");
    let _proxy_guard = EnvGuard::remove("RSYNC_PROXY");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new("example.com".to_string(), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("connect program listing succeeds");
    assert_eq!(list.entries().len(), 1);
    let entry = &list.entries()[0];
    assert_eq!(entry.name(), "example");
    assert_eq!(entry.comment(), Some("example.com:873"));
}

#[test]
fn connect_program_token_expansion_matches_upstream_rules() {
    let template = OsString::from("netcat %H %P %%");
    let config = ConnectProgramConfig::new(template, None).expect("config");
    let rendered = config
        .format_command("daemon.example", 10873)
        .expect("rendered command");

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(rendered.as_bytes(), b"netcat daemon.example 10873 %");
    }

    #[cfg(not(unix))]
    {
        assert_eq!(rendered, OsString::from("netcat daemon.example 10873 %"));
    }
}

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_port_option() {
    let _lock = env_lock().lock().expect("env mutex poisoned");
    let temp = tempdir().expect("tempdir created");
    let capture_path = temp.path().join("args.txt");
    let script_path = temp.path().join("capture.sh");
    let script_contents = capture_args_script();
    fs::write(&script_path, script_contents).expect("script written");
    let metadata = fs::metadata(&script_path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("script permissions set");

    let mut args = baseline_fallback_args();
    args.fallback_binary = Some(script_path.clone().into_os_string());
    args.port = Some(10_873);
    args.remainder = vec![OsString::from(format!(
        "CAPTURE={}",
        capture_path.display()
    ))];

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
        .expect("fallback invocation succeeds");

    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let captured = fs::read_to_string(&capture_path).expect("capture contents");
    assert!(captured.lines().any(|line| line == "--port=10873"));
}

#[test]
fn run_module_list_collects_motd_after_acknowledgement() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: OK\n",
        "@RSYNCD: MOTD: Post-acknowledgement notice\n",
        "gamma\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[String::from("Post-acknowledgement notice")]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "gamma");
    assert!(list.entries()[0].comment().is_none());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_suppresses_motd_when_requested() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list =
        run_module_list_with_options(request, ModuleListOptions::default().suppress_motd(true))
            .expect("module list succeeds");
    assert!(list.motd_lines().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_warnings() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@WARNING: Maintenance scheduled\n",
        "@RSYNCD: OK\n",
        "delta\n",
        "@WARNING: Additional notice\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "delta");
    assert_eq!(
        list.warnings(),
        &[
            String::from("Maintenance scheduled"),
            String::from("Additional notice")
        ]
    );
    assert!(list.capabilities().is_empty());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_capabilities() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: CAP modules uid\n",
        "@RSYNCD: OK\n",
        "epsilon\n",
        "@RSYNCD: CAP compression\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "epsilon");
    assert_eq!(
        list.capabilities(),
        &[String::from("modules uid"), String::from("compression")]
    );

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_via_proxy_connects_through_tunnel() {
    let responses = vec!["@RSYNCD: OK\n", "theta\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, DEFAULT_PROXY_STATUS_LINE);

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
    assert_eq!(list.entries()[0].name(), "theta");

    let captured = request_rx.recv().expect("proxy request");
    assert!(
        captured
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("CONNECT "))
    );

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_via_proxy_includes_auth_header() {
    let responses = vec!["@RSYNCD: OK\n", "iota\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let expected_header = "Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=";
    let (proxy_addr, request_rx, proxy_handle) = spawn_stub_proxy(
        daemon_addr,
        Some(expected_header),
        DEFAULT_PROXY_STATUS_LINE,
    );

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("user:secret@{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "iota");

    let captured = request_rx.recv().expect("proxy request");
    assert!(captured.contains(expected_header));

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

