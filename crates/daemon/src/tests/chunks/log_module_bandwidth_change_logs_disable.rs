#[test]
fn log_module_bandwidth_change_logs_disable() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path, Brand::Oc).expect("open log");

    log_module_bandwidth_change(
        &log,
        Some("client.example"),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        None,
        LimiterChange::Disabled,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("removed bandwidth limit"));
    assert!(contents.contains("client.example"));
}

