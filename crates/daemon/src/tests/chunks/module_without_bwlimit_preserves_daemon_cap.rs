#[test]
fn module_without_bwlimit_preserves_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, false, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("daemon cap should remain active");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

