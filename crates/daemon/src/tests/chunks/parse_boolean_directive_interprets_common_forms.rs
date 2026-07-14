#[test]
fn parse_boolean_directive_interprets_common_forms() {
    for value in ["1", "true", "YES"] {
        assert_eq!(parse_boolean_directive(value), Some(true));
    }

    for value in ["0", "false", "No"] {
        assert_eq!(parse_boolean_directive(value), Some(false));
    }

    // upstream: loadparm.c:363-376 set_boolean() accepts only yes/true/1 and
    // no/false/0 - on/off are never recognized, so they are malformed (None),
    // like any other unrecognized value.
    for value in [" On ", " off ", "maybe"] {
        assert_eq!(parse_boolean_directive(value), None);
    }
}
