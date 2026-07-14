#[test]
fn parse_max_connections_directive_handles_zero_and_positive() {
    // upstream: `max connections` is a P_INTEGER directive read via atoi()
    // (loadparm.c:431-433). atoi maps an empty, whitespace-only, non-numeric,
    // or non-positive value to <= 0, which means unlimited -> Some(None).
    assert_eq!(parse_max_connections_directive(""), Some(None));
    assert_eq!(parse_max_connections_directive("  "), Some(None));
    assert_eq!(parse_max_connections_directive("0"), Some(None));
    assert_eq!(parse_max_connections_directive("-1"), Some(None));
    assert_eq!(parse_max_connections_directive("invalid"), Some(None));

    let expected = NonZeroU32::new(25).expect("non-zero");
    assert_eq!(parse_max_connections_directive("25"), Some(Some(expected)));
}
