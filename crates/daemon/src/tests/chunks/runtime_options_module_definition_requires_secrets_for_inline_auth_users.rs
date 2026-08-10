#[test]
fn runtime_options_module_definition_requires_secrets_for_inline_auth_users() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("logs=/var/log;auth-users=alice"),
    ])
    .expect_err("missing secrets file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("did not supply a secrets file")
    );
}

