// upstream: loadparm.c:401 strwiEQ skips only whitespace, so a hyphen is
// significant and `read-only` names no parameter at all.
#[test]
fn runtime_options_module_definition_refuses_hyphenated_parameter_names() {
    for option in ["read-only=yes", "incoming-chmod=Fx"] {
        let error = RuntimeOptions::parse(&[
            OsString::from("--module"),
            OsString::from(format!("docs=/srv/docs;{option}")),
        ])
        .expect_err("hyphenated parameter name should be refused");
        let key = option.split_once('=').expect("option has '='").0;
        assert!(
            error
                .message()
                .to_string()
                .contains(&format!("unsupported module option '{key}'")),
            "unexpected error for {option}: {}",
            error.message()
        );
    }
}
