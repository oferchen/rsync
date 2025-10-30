#[test]
fn runtime_options_ipv4_rejects_ipv6_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv4"),
    ])
    .expect_err("ipv6 bind with --ipv4 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv4 with an IPv6 bind address")
    );
}

