#[test]
fn module_bwlimit_unlimited_with_explicit_burst_preserves_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
    ));

    let burst = NonZeroU64::new(256 * 1024).unwrap();
    let change = apply_module_bandwidth_limit(&mut limiter, None, false, true, Some(burst), true);

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("daemon cap should remain active");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

