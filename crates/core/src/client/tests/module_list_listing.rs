#[test]
fn run_module_list_accepts_plaintext_motd_before_acknowledgment() {
    let responses = vec![
        "-----\n",
        "Welcome to the stub rsync service\n",
        "@RSYNCD: OK\n",
        "public\tExample module\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");

    assert_eq!(list.motd_lines(), ["-----", "Welcome to the stub rsync service"]);
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "public");
    assert_eq!(list.entries()[0].comment(), Some("Example module"));

    handle.join().expect("daemon thread completes");
}

#[test]
fn run_module_list_suppresses_plaintext_motd_when_requested() {
    let responses = vec![
        "Banner headline\n",
        "@RSYNCD: OK\n",
        "archive\tRotating snapshots\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let options = ModuleListOptions::default().suppress_motd(true);
    let list = run_module_list_with_options(request, options).expect("module list succeeds");

    assert!(list.motd_lines().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "archive");
    assert_eq!(list.entries()[0].comment(), Some("Rotating snapshots"));

    handle.join().expect("daemon thread completes");
}
