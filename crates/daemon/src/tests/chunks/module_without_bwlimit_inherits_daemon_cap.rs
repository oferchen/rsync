#[test]
fn module_without_bwlimit_inherits_daemon_cap() {
    let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(limit));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, false, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("limiter remains in effect");
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
}

