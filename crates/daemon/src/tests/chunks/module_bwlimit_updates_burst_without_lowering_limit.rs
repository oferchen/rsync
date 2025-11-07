#[test]
fn module_bwlimit_updates_burst_without_lowering_limit() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(4 * 1024 * 1024),
        true,
        true,
        Some(NonZeroU64::new(512 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(512 * 1024).unwrap())
    );
}

