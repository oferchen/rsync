#[test]
fn module_bwlimit_configures_unlimited_daemon() {
    let mut limiter = None;

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(2 * 1024 * 1024),
        true,
        true,
        None,
        false,
    );

    assert_eq!(change, LimiterChange::Enabled);

    let limiter = limiter.expect("limiter configured by module");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());

    let mut limiter = Some(limiter);
    let change = apply_module_bandwidth_limit(
        &mut limiter,
        None,
        false,
        true,
        Some(NonZeroU64::new(256 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);
    let limiter = limiter.expect("limiter preserved");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(256 * 1024).unwrap())
    );
}

