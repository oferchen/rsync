#[test]
fn sanitize_module_identifier_replaces_control_characters() {
    let ident = "bad\nname\t";
    let sanitized = sanitize_module_identifier(ident);
    assert_eq!(sanitized.as_ref(), "bad?name?");
}

