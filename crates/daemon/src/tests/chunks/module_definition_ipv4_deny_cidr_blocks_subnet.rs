#[test]
fn module_definition_ipv4_deny_cidr_blocks_subnet() {
    let module = module_with_host_patterns(&[], &["172.16.0.0/12"]);
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 254)), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1)), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
}
