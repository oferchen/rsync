#[test]
fn parse_max_connections_directive_handles_zero_and_positive() {
    assert_eq!(parse_max_connections_directive(""), None);
    assert_eq!(parse_max_connections_directive("  "), None);
    assert_eq!(parse_max_connections_directive("0"), Some(None));

    let expected = NonZeroU32::new(25).expect("non-zero");
    assert_eq!(parse_max_connections_directive("25"), Some(Some(expected)));

    assert_eq!(parse_max_connections_directive("-1"), None);
    assert_eq!(parse_max_connections_directive("invalid"), None);
}

