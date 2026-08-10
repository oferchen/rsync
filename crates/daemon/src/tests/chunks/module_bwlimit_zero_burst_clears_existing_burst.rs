#[test]
fn module_bwlimit_zero_burst_clears_existing_burst() {
    let mut limiter = Some(BandwidthLimiter::with_burst(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
        Some(NonZeroU64::new(512 * 1024).unwrap()),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(4 * 1024 * 1024),
        true,
        true,
        None,
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

