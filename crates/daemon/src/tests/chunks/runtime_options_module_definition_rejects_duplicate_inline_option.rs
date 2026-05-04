#[test]
fn runtime_options_module_definition_rejects_duplicate_inline_option() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs;read-only=yes;read-only=no"),
    ])
    .expect_err("duplicate inline option should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate module option")
    );
}

