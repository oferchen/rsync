#[test]
fn module_definition_ipv4_allow_matches_exact() {
    let module = module_with_host_patterns(&["192.168.1.1"], &[]);
    let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    let denied = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
    assert!(module.permits(allowed, None));
    assert!(!module.permits(denied, None));
}
