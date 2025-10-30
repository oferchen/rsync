#[test]
fn runtime_options_bind_accepts_bracketed_ipv6() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("[::1]")])
        .expect("parse bracketed ipv6");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

