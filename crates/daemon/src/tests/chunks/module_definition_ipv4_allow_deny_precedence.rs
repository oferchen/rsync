#[test]
fn module_definition_ipv4_allow_deny_precedence() {
    // upstream: access.c - hosts_allow is checked first (must match), then
    // hosts_deny (must not match). A peer in both lists is denied.
    let module = module_with_host_patterns(&["192.168.0.0/16"], &["192.168.1.0/24"]);
    // In the allowed subnet but not in the denied subnet - permitted.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), None));
    // In both the allowed and denied subnets - denied.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), None));
    // Outside the allowed subnet entirely - denied by allow check.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
}
