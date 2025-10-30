#[test]
fn runtime_options_ipv6_sets_default_bind_address() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--ipv6")]).expect("parse --ipv6 succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

