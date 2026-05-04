#[test]
fn parse_boolean_directive_interprets_common_forms() {
    for value in ["1", "true", "YES", " On "] {
        assert_eq!(parse_boolean_directive(value), Some(true));
    }

    for value in ["0", "false", "No", " off "] {
        assert_eq!(parse_boolean_directive(value), Some(false));
    }

    assert_eq!(parse_boolean_directive("maybe"), None);
}

