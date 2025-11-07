#[test]
fn log_module_bandwidth_change_logs_updates() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path).expect("open log");
    let limiter = BandwidthLimiter::with_burst(
        NonZeroU64::new(8 * 1024).expect("limit"),
        Some(NonZeroU64::new(64 * 1024).expect("burst")),
    );

    log_module_bandwidth_change(
        &log,
        None,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        Some(&limiter),
        LimiterChange::Enabled,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("enabled bandwidth limit 8 KiB/s with burst 64 KiB/s"));
    assert!(contents.contains("module 'docs'"));
    assert!(contents.contains("127.0.0.1"));
}

