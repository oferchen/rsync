#[test]
fn module_bwlimit_unlimited_with_burst_override_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, true, true, None, true);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

