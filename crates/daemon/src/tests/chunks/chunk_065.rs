#[test]
fn runtime_options_rejects_ipv4_ipv6_combo() {
    let error = RuntimeOptions::parse(&[OsString::from("--ipv4"), OsString::from("--ipv6")])
        .expect_err("conflicting address families should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot combine --ipv4 with --ipv6")
    );
}

