#[test]
fn runtime_options_bind_resolves_hostnames() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("localhost")])
        .expect("parse hostname bind");

    let address = options.bind_address();
    assert!(
        address == IpAddr::V4(Ipv4Addr::LOCALHOST) || address == IpAddr::V6(Ipv6Addr::LOCALHOST),
        "unexpected resolved address {address}",
    );
}

