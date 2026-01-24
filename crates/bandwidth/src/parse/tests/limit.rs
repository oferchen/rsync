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

// ==================== Additional coverage tests for bandwidth limit components ====================

#[test]
fn constrained_by_clears_burst_when_override_is_unlimited() {
    // Test lines 218-224 in components.rs
    // When override is unlimited (limit_specified=true, rate=None),
    // the result should be unlimited with burst cleared
    let client = parse_bandwidth_limit("4M:48K").expect("client components");

    // Create an unlimited override with limit_specified = true
    let module = BandwidthLimitComponents::new_with_flags(None, None, true, false);

    let combined = client.constrained_by(&module);

    assert!(combined.is_unlimited());
    assert!(combined.burst().is_none());
    assert!(!combined.burst_specified());
}

#[test]
fn constrained_by_burst_only_override_on_unlimited_client() {
    // Test burst-only override when client is unlimited
    // This should not apply burst since there's no active limit
    let client = BandwidthLimitComponents::unlimited();
    let burst = NonZeroU64::new(8192).unwrap();
    let module = BandwidthLimitComponents::new_with_flags(None, Some(burst), false, true);

    let combined = client.constrained_by(&module);

    // Client is unlimited, burst-only override doesn't create a limiter
    assert!(combined.is_unlimited());
    assert!(combined.burst().is_none());
}

#[test]
fn constrained_by_preserves_client_burst_when_override_has_no_burst() {
    // When override has rate but no burst specified, preserve client burst
    let client = parse_bandwidth_limit("8M:64K").expect("client");
    let module = BandwidthLimitComponents::new(Some(NonZeroU64::new(4 * 1024 * 1024).unwrap()), None);

    let combined = client.constrained_by(&module);

    // Rate should be capped to 4M
    assert_eq!(combined.rate().map(NonZeroU64::get), Some(4 * 1024 * 1024));
    // Burst should be preserved from client
    assert_eq!(combined.burst().map(NonZeroU64::get), Some(64 * 1024));
}

#[test]
fn constrained_by_clears_client_burst_when_override_has_rate_and_client_was_unlimited() {
    // Lines 217-219: when override has rate but client was unlimited, clear burst
    let client = BandwidthLimitComponents::unlimited();
    let rate = NonZeroU64::new(2 * 1024 * 1024).unwrap();
    let module = BandwidthLimitComponents::new(Some(rate), None);

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate(), Some(rate));
    assert!(combined.burst().is_none());
    assert!(!combined.burst_specified());
}

#[test]
fn constrained_by_takes_stricter_limit_with_explicit_burst_clear() {
    // When override specifies rate AND burst (even if burst is None with burst_specified=true)
    let client = parse_bandwidth_limit("8M:64K").expect("client");
    // Override with lower rate and explicit burst=None
    let module = BandwidthLimitComponents::new_with_specified(
        Some(NonZeroU64::new(2 * 1024 * 1024).unwrap()),
        None,
        true, // burst_specified = true, but burst = None
    );

    let combined = client.constrained_by(&module);

    // Rate should be min(8M, 2M) = 2M
    assert_eq!(combined.rate().map(NonZeroU64::get), Some(2 * 1024 * 1024));
    // Burst should be cleared because override specified burst=None
    assert!(combined.burst().is_none());
    assert!(combined.burst_specified());
}

#[test]
fn parse_bandwidth_limit_with_burst_zero_clears_burst() {
    // ":0" burst should be parsed as None with burst_specified=true
    let components = parse_bandwidth_limit("2M:0").expect("parse succeeds");
    assert_eq!(components.rate(), NonZeroU64::new(2 * 1024 * 1024));
    assert!(components.burst().is_none());
    assert!(components.burst_specified());
}

#[test]
fn parse_bandwidth_limit_with_very_small_burst() {
    // Burst can be small (it's not subject to minimum like rate)
    let error = parse_bandwidth_limit("1M:100b").unwrap_err();
    // 100 bytes is below 512 minimum
    assert_eq!(error, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_limit_with_large_burst() {
    // Large burst value
    let components = parse_bandwidth_limit("1M:10M").expect("parse succeeds");
    assert_eq!(components.rate(), NonZeroU64::new(1024 * 1024));
    assert_eq!(components.burst(), NonZeroU64::new(10 * 1024 * 1024));
}

#[test]
fn components_to_limiter_creates_correct_limiter() {
    let components = BandwidthLimitComponents::new(
        Some(NonZeroU64::new(2048).unwrap()),
        Some(NonZeroU64::new(1024).unwrap()),
    );

    let limiter = components.to_limiter().expect("should create limiter");
    assert_eq!(limiter.limit_bytes().get(), 2048);
    assert_eq!(limiter.burst_bytes().map(|b| b.get()), Some(1024));
}

#[test]
fn components_equality_includes_specification_flags() {
    let c1 = BandwidthLimitComponents::new_with_flags(
        Some(NonZeroU64::new(1024).unwrap()),
        None,
        true,
        false,
    );
    let c2 = BandwidthLimitComponents::new_with_flags(
        Some(NonZeroU64::new(1024).unwrap()),
        None,
        true,
        true, // Different burst_specified
    );

    // These should NOT be equal because burst_specified differs
    assert_ne!(c1, c2);
}

#[test]
fn components_copy_trait() {
    let c1 = BandwidthLimitComponents::new(
        Some(NonZeroU64::new(1024).unwrap()),
        None,
    );
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
    let c3 = BandwidthLimitComponents::new_with_flags(None, None, true, false);
    let combined2 = c1.constrained_by(&c3);
    assert!(combined2.limit_specified());
}

#[test]
fn parse_bandwidth_limit_various_rate_burst_combinations() {
    // Small rate, large burst
    let small_rate_large_burst = parse_bandwidth_limit("1K:1M").expect("parse");
    assert_eq!(small_rate_large_burst.rate(), NonZeroU64::new(1024));
    assert_eq!(small_rate_large_burst.burst(), NonZeroU64::new(1024 * 1024));

    // Large rate, small burst
    let large_rate_small_burst = parse_bandwidth_limit("100M:1K").expect("parse");
    assert_eq!(large_rate_small_burst.rate(), NonZeroU64::new(100 * 1024 * 1024));
    assert_eq!(large_rate_small_burst.burst(), NonZeroU64::new(1024));
}
