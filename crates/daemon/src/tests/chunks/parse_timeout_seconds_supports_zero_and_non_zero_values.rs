#[test]
fn parse_timeout_seconds_supports_zero_and_non_zero_values() {
    // upstream: `timeout` is a P_INTEGER directive read via atoi()
    // (loadparm.c:431-433). atoi maps an empty, whitespace-only, non-numeric,
    // or non-positive value to <= 0, which disables the timeout -> Some(None).
    assert_eq!(parse_timeout_seconds(""), Some(None));
    assert_eq!(parse_timeout_seconds("  "), Some(None));
    assert_eq!(parse_timeout_seconds("0"), Some(None));
    assert_eq!(parse_timeout_seconds("invalid"), Some(None));

    let expected = NonZeroU64::new(30).expect("non-zero timeout");
    assert_eq!(parse_timeout_seconds("30"), Some(Some(expected)));
}
