use super::{
    BandwidthLimitComponents, BandwidthLimiter, BandwidthParseError, LimiterChange, NonZeroU64,
    parse_bandwidth_limit,
};

#[test]
fn parse_bandwidth_limit_accepts_unlimited_rate() {
    let components: BandwidthLimitComponents = "0".parse().expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_rejects_colon() {
    // A colon is not valid size syntax; RATE:BURST is no longer accepted.
    let error = parse_bandwidth_limit("1M:64K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
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
    assert!(!unlimited.limit_specified());
}

#[test]
fn parse_bandwidth_limit_rejects_surrounding_whitespace() {
    let error = parse_bandwidth_limit(" 1M ").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn components_into_limiter_respects_rate() {
    let components = BandwidthLimitComponents::new(NonZeroU64::new(1024));
    let limiter = components.into_limiter().expect("limiter");
    assert_eq!(limiter.limit_bytes().get(), 1024);
}

#[test]
fn components_apply_to_limiter_disables_when_explicitly_unlimited() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024).expect("limit"),
    ));

    let components = BandwidthLimitComponents::new_with_flags(None, true);
    let change = components.apply_to_limiter(&mut limiter);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn new_with_flags_forces_limit_specified_when_rate_present() {
    let limit = NonZeroU64::new(2048).expect("limit");
    let components = BandwidthLimitComponents::new_with_flags(Some(limit), false);

    assert!(components.limit_specified());
    assert_eq!(components.rate(), Some(limit));
}

#[test]
fn components_into_limiter_returns_none_when_unlimited() {
    let components = BandwidthLimitComponents::new(None);
    assert!(components.into_limiter().is_none());
}

#[test]
fn parse_bandwidth_limit_rejects_trailing_garbage() {
    let error = parse_bandwidth_limit("1M extra").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn constrained_by_applies_override_rate_when_client_unlimited() {
    // When the override supplies a rate but the client was previously
    // unlimited, the combined rate becomes the override rate.
    let client = BandwidthLimitComponents::unlimited();
    let rate = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let module = BandwidthLimitComponents::new(Some(rate));

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate(), Some(rate));
}

#[test]
fn components_to_limiter_creates_correct_limiter() {
    let components = BandwidthLimitComponents::new(Some(NonZeroU64::new(2048).unwrap()));

    let limiter = components.to_limiter().expect("should create limiter");
    assert_eq!(limiter.limit_bytes().get(), 2048);
}

#[test]
fn components_copy_trait() {
    let c1 = BandwidthLimitComponents::new(Some(NonZeroU64::new(1024).unwrap()));
    let c2 = c1; // Copy
    let c3 = c1; // Another copy

    assert_eq!(c1, c2);
    assert_eq!(c2, c3);
}

#[test]
fn constrained_by_combines_limit_specified_flags() {
    // Test that limit_specified is OR'd from both components
    let c1 = BandwidthLimitComponents::unlimited();
    let c2 = BandwidthLimitComponents::unlimited();

    let combined = c1.constrained_by(&c2);
    assert!(!combined.limit_specified());

    // Now with one specifying limit
    let c3 = BandwidthLimitComponents::new_with_flags(None, true);
    let combined2 = c1.constrained_by(&c3);
    assert!(combined2.limit_specified());
}
