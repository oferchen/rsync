#[test]
fn module_bwlimit_burst_does_not_raise_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(8 * 1024 * 1024),
        true,
        true,
        Some(NonZeroU64::new(256 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(256 * 1024).unwrap())
    );
}

