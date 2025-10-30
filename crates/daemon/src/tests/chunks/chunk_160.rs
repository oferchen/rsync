#[test]
fn module_bwlimit_unlimited_is_noop_when_no_cap() {
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_module_bandwidth_limit(&mut limiter, None, true, true, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    assert!(limiter.is_none());
}

