use super::prelude::*;


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

