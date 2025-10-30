use super::prelude::*;


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

