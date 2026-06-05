#[test]
fn module_definition_ipv4_deny_blocks_matching_ip() {
    let module = module_with_host_patterns(&[], &["192.168.1.100"]);
    let blocked = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
    let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101));
    assert!(!module.permits(blocked, None));
    assert!(module.permits(allowed, None));
}
