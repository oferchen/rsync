#[test]
fn runtime_options_ipv6_rejects_ipv4_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("127.0.0.1"),
        OsString::from("--ipv6"),
    ])
    .expect_err("ipv4 bind with --ipv6 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv6 with an IPv4 bind address")
    );
}

