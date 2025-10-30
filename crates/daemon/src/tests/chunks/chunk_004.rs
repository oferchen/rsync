#[test]
fn parse_timeout_seconds_supports_zero_and_non_zero_values() {
    assert_eq!(parse_timeout_seconds(""), None);
    assert_eq!(parse_timeout_seconds("  "), None);
    assert_eq!(parse_timeout_seconds("0"), Some(None));

    let expected = NonZeroU64::new(30).expect("non-zero timeout");
    assert_eq!(parse_timeout_seconds("30"), Some(Some(expected)));
    assert_eq!(parse_timeout_seconds("invalid"), None);
}

