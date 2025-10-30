#[test]
fn runtime_options_ipv6_accepts_ipv6_bind_address() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv6"),
    ])
    .expect("ipv6 bind succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

