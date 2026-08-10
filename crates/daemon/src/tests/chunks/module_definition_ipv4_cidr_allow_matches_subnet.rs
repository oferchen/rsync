#[test]
fn module_definition_ipv4_cidr_allow_matches_subnet() {
    let module = module_with_host_patterns(&["10.0.0.0/8"], &[]);
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), None));
}
