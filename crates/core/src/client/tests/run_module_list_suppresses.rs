use super::prelude::*;


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

