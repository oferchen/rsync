#[test]
fn module_bwlimit_configured_unlimited_without_specified_flag_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, true, None, false);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

