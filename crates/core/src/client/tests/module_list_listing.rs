#[test]
fn run_module_list_accepts_plaintext_motd_before_acknowledgment() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

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
fn run_module_list_preserves_empty_comment_marker_for_upstream_format() {
    // upstream emits `%-15s\t%s\n` for every module, including those without a
    // configured comment. The client must preserve the trailing tab so the
    // rendered output matches upstream byte-for-byte. This regression covers
    // the upstream `daemon.test` expectation for the `test-scratch` module.
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: OK\n",
        "test-from      \tr/o\n",
        "test-to        \tr/w\n",
        "test-scratch   \t\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");

    assert_eq!(list.entries().len(), 3);
    assert_eq!(list.entries()[0].name(), "test-from      ");
    assert_eq!(list.entries()[0].comment(), Some("r/o"));
    assert_eq!(list.entries()[1].name(), "test-to        ");
    assert_eq!(list.entries()[1].comment(), Some("r/w"));
    assert_eq!(list.entries()[2].name(), "test-scratch   ");
    assert_eq!(list.entries()[2].comment(), Some(""));

    handle.join().expect("daemon thread completes");
}

#[test]
fn run_module_list_suppresses_plaintext_motd_when_requested() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

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
