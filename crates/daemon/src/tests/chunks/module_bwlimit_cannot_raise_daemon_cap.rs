#[test]
fn module_bwlimit_cannot_raise_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(8 * 1024 * 1024),
        true,
        true,
        None,
        false,
    );

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

