use super::prelude::*;


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

