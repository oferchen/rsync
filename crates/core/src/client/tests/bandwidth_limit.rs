use super::prelude::*;


#[test]
fn bandwidth_limit_from_components_returns_none_for_unlimited() {
    let components = bandwidth::BandwidthLimitComponents::unlimited();
    assert!(BandwidthLimit::from_components(components).is_none());
}


#[test]
fn bandwidth_limit_from_components_preserves_rate_and_burst() {
    let rate = NonZeroU64::new(8 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(64 * 1024).expect("non-zero");
    let components = bandwidth::BandwidthLimitComponents::new(Some(rate), Some(burst));
    let limit = BandwidthLimit::from_components(components).expect("limit produced");

    assert_eq!(limit.bytes_per_second(), rate);
    assert_eq!(limit.burst_bytes(), Some(burst));
    assert!(limit.burst_specified());
}


#[test]
fn bandwidth_limit_components_conversion_preserves_configuration() {
    let rate = NonZeroU64::new(12 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(256 * 1024).expect("non-zero");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let components = limit.components();
    assert_eq!(components.rate(), Some(rate));
    assert_eq!(components.burst(), Some(burst));
    assert!(components.burst_specified());

    let round_trip = BandwidthLimit::from_components(components).expect("limit produced");
    assert_eq!(round_trip, limit);
}


#[test]
fn bandwidth_limit_into_components_supports_from_trait() {
    let rate = NonZeroU64::new(4 * 1024).expect("non-zero");
    let burst = NonZeroU64::new(32 * 1024).expect("non-zero");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let via_ref: bandwidth::BandwidthLimitComponents = (&limit).into();
    let via_value: bandwidth::BandwidthLimitComponents = limit.into();

    assert_eq!(via_ref.rate(), Some(rate));
    assert_eq!(via_ref.burst(), Some(burst));
    assert_eq!(via_value.rate(), Some(rate));
    assert_eq!(via_value.burst(), Some(burst));
}


#[test]
fn bandwidth_limit_to_limiter_preserves_rate_and_burst() {
    let rate = NonZeroU64::new(8 * 1024 * 1024).expect("non-zero rate");
    let burst = NonZeroU64::new(256 * 1024).expect("non-zero burst");
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    let limiter = limit.to_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}


#[test]
fn bandwidth_limit_into_limiter_transfers_configuration() {
    let rate = NonZeroU64::new(4 * 1024 * 1024).expect("non-zero rate");
    let limit = BandwidthLimit::from_rate_and_burst(rate, None);

    let limiter = limit.into_limiter();

    assert_eq!(limiter.limit_bytes(), rate);
    assert_eq!(limiter.burst_bytes(), None);
}


#[test]
fn bandwidth_limit_fallback_argument_returns_bytes_per_second() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(2048).unwrap());
    assert_eq!(limit.fallback_argument(), OsString::from("2048"));
}


#[test]
fn bandwidth_limit_fallback_argument_includes_burst_when_specified() {
    let rate = NonZeroU64::new(8 * 1024).unwrap();
    let burst = NonZeroU64::new(32 * 1024).unwrap();
    let limit = BandwidthLimit::from_rate_and_burst(rate, Some(burst));

    assert_eq!(limit.fallback_argument(), OsString::from("8192:32768"));
}


#[test]
fn bandwidth_limit_fallback_argument_preserves_explicit_zero_burst() {
    let rate = NonZeroU64::new(4 * 1024).unwrap();
    let components =
        bandwidth::BandwidthLimitComponents::new_with_specified(Some(rate), None, true);

    let limit = BandwidthLimit::from_components(components).expect("limit produced");

    assert!(limit.burst_specified());
    assert_eq!(limit.burst_bytes(), None);
    assert_eq!(limit.fallback_argument(), OsString::from("4096:0"));

    let round_trip = limit.components();
    assert!(round_trip.burst_specified());
    assert_eq!(round_trip.burst(), None);
}


#[test]
fn bandwidth_limit_fallback_unlimited_argument_returns_zero() {
    assert_eq!(
        BandwidthLimit::fallback_unlimited_argument(),
        OsString::from("0"),
    );
}

