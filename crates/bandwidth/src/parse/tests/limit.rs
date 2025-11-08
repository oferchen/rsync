use super::{
    BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange, NonZeroU64,
    parse_bandwidth_limit,
};

#[test]
fn parse_bandwidth_limit_accepts_burst_component() {
    let components = parse_bandwidth_limit("1M:64K").expect("parse succeeds");
    assert_eq!(
        components,
        BandwidthLimitComponents::new(NonZeroU64::new(1_048_576), NonZeroU64::new(64 * 1024),)
    );
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_accepts_unlimited_rate() {
    let components: BandwidthLimitComponents = "0".parse().expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_rejects_invalid_burst() {
    let error = parse_bandwidth_limit("1M:abc").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_negative_burst() {
    let error = parse_bandwidth_limit("1M:-64K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_zero_rate_disables_burst() {
    let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
    assert!(components.burst().is_none());
}

#[test]
fn components_discard_burst_for_unlimited_rate() {
    let burst = NonZeroU64::new(4096);
    let components = BandwidthLimitComponents::new(None, burst);
    assert!(components.is_unlimited());
    assert_eq!(components.burst(), None);
}

#[test]
fn parse_bandwidth_limit_reports_unlimited_state() {
    let components = parse_bandwidth_limit("0").expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
    let limited = parse_bandwidth_limit("1M").expect("parse succeeds");
    assert!(!limited.is_unlimited());
    assert!(limited.limit_specified());
}

#[test]
fn bandwidth_limit_components_unlimited_matches_default() {
    let unlimited = BandwidthLimitComponents::unlimited();
    assert!(unlimited.is_unlimited());
    assert_eq!(unlimited, BandwidthLimitComponents::default());
    assert!(!unlimited.burst_specified());
    assert!(!unlimited.limit_specified());
}

#[test]
fn parse_bandwidth_limit_accepts_zero_burst() {
    let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
    assert_eq!(
        components,
        BandwidthLimitComponents::new_with_specified(NonZeroU64::new(1_048_576), None, true,)
    );
    assert!(components.burst_specified());
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_rejects_surrounding_whitespace() {
    let error = parse_bandwidth_limit(" 1M ").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn components_into_limiter_respects_rate_and_burst() {
    let components = BandwidthLimitComponents::new(NonZeroU64::new(1024), NonZeroU64::new(4096));
    let limiter = components.into_limiter().expect("limiter");
    assert_eq!(limiter.limit_bytes().get(), 1024);
    assert_eq!(limiter.burst_bytes().map(NonZeroU64::get), Some(4096));
}

#[test]
fn constrained_by_prefers_stricter_limit_and_preserves_burst() {
    let client = parse_bandwidth_limit("8M:64K").expect("client components");
    let module = parse_bandwidth_limit("2M").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(2 * 1024 * 1024));
    assert_eq!(combined.burst().map(NonZeroU64::get), Some(64 * 1024));
    assert!(combined.burst_specified());
    assert!(combined.limit_specified());
}

#[test]
fn constrained_by_initialises_limiter_when_unlimited() {
    let client = BandwidthLimitComponents::unlimited();
    let module = parse_bandwidth_limit("1M:32K").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(1024 * 1024));
    assert_eq!(combined.burst().map(NonZeroU64::get), Some(32 * 1024));
    assert!(combined.burst_specified());
    assert!(combined.limit_specified());
}

#[test]
fn constrained_by_respects_module_disable() {
    let client = parse_bandwidth_limit("4M:48K").expect("client components");
    let module = parse_bandwidth_limit("0").expect("module components");

    let combined = client.constrained_by(&module);

    assert!(combined.is_unlimited());
    assert!(combined.limit_specified());
    assert!(!combined.burst_specified());
    assert!(combined.burst().is_none());
}

#[test]
fn constrained_by_overrides_burst_when_specified() {
    let client = parse_bandwidth_limit("3M:16K").expect("client components");
    let module = parse_bandwidth_limit("5M:0").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(3 * 1024 * 1024));
    assert!(combined.burst().is_none());
    assert!(combined.burst_specified());
}

#[test]
fn components_apply_to_limiter_disables_when_explicitly_unlimited() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024).expect("limit"),
    ));

    let components = BandwidthLimitComponents::new_with_flags(None, None, true, false);
    let change = components.apply_to_limiter(&mut limiter);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn components_apply_to_limiter_updates_burst_with_unlimited_override() {
    let limit = NonZeroU64::new(4 * 1024 * 1024).expect("limit");
    let mut limiter = Some(BandwidthLimiter::new(limit));
    let burst = NonZeroU64::new(256 * 1024).expect("burst");

    let components = BandwidthLimitComponents::new_with_flags(None, Some(burst), false, true);
    let change = components.apply_to_limiter(&mut limiter);

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn new_with_flags_retains_unlimited_burst_specification() {
    let burst = NonZeroU64::new(128 * 1024).expect("burst");
    let components = BandwidthLimitComponents::new_with_flags(None, Some(burst), false, true);

    assert!(!components.limit_specified());
    assert!(components.burst_specified());
    assert_eq!(components.burst(), Some(burst));
    assert!(components.is_unlimited());
}

#[test]
fn new_with_flags_forces_limit_specified_when_rate_present() {
    let limit = NonZeroU64::new(2048).expect("limit");
    let components = BandwidthLimitComponents::new_with_flags(Some(limit), None, false, false);

    assert!(components.limit_specified());
    assert_eq!(components.rate(), Some(limit));
    assert!(components.burst().is_none());
}

#[test]
fn new_with_flags_tracks_explicit_burst_clearing_with_rate() {
    let limit = NonZeroU64::new(4096).expect("limit");
    let components = BandwidthLimitComponents::new_with_flags(Some(limit), None, false, true);

    assert!(components.limit_specified());
    assert!(components.burst_specified());
    assert!(components.burst().is_none());
}

#[test]
fn components_into_limiter_returns_none_when_unlimited() {
    let components = BandwidthLimitComponents::new(None, NonZeroU64::new(4096));
    assert!(components.into_limiter().is_none());
}

#[test]
fn parse_bandwidth_limit_records_explicit_burst_presence() {
    let specified = parse_bandwidth_limit("2M:128K").expect("parse succeeds");
    assert!(specified.burst_specified());
    assert_eq!(specified.burst(), NonZeroU64::new(128 * 1024));

    let unspecified = parse_bandwidth_limit("2M").expect("parse succeeds");
    assert!(!unspecified.burst_specified());
    assert!(unspecified.burst().is_none());
}

#[test]
fn parse_bandwidth_limit_rejects_whitespace_around_burst_separator() {
    let error = parse_bandwidth_limit("4M : 256K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_trailing_garbage() {
    let error = parse_bandwidth_limit("1M extra").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_missing_burst_value() {
    let error = parse_bandwidth_limit("1M:").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}
