#[test]
fn log_module_bandwidth_change_ignores_unchanged() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path).expect("open log");

    let limiter = BandwidthLimiter::new(NonZeroU64::new(4 * 1024).expect("limit"));

    log_module_bandwidth_change(
        &log,
        None,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        Some(&limiter),
        LimiterChange::Unchanged,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.is_empty());
}

