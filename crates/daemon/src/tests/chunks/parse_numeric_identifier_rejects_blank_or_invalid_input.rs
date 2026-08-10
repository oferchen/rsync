#[test]
fn parse_numeric_identifier_rejects_blank_or_invalid_input() {
    assert_eq!(parse_numeric_identifier("  42  "), Some(42));
    assert_eq!(parse_numeric_identifier(""), None);
    assert_eq!(parse_numeric_identifier("not-a-number"), None);
}

