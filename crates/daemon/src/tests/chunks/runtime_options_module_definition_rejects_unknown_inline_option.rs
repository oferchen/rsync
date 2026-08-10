#[test]
fn runtime_options_module_definition_rejects_unknown_inline_option() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs;unknown=true"),
    ])
    .expect_err("unknown option should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("unsupported module option")
    );
}

