#[test]
fn module_definition_ipv6_cidr_allow_matches_prefix() {
    let module = module_with_host_patterns(&["fd00::/8"], &[]);
    assert!(module.permits(
        IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
        None,
    ));
    assert!(module.permits(
        IpAddr::V6(Ipv6Addr::new(0xfdff, 0xffff, 0, 0, 0, 0, 0, 1)),
        None,
    ));
    assert!(!module.permits(
        IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
        None,
    ));
}
