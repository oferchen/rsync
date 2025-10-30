#[test]
fn sanitize_module_identifier_preserves_clean_input() {
    let ident = "secure-module";
    match sanitize_module_identifier(ident) {
        Cow::Borrowed(value) => assert_eq!(value, ident),
        Cow::Owned(_) => panic!("clean identifiers should not allocate"),
    }
}

