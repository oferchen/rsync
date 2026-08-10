#[test]
fn module_definition_only_allow_denies_non_matching() {
    // upstream: access.c - when only hosts_allow is set, anything not allowed
    // is denied.
    let module = module_with_host_patterns(&["192.168.1.0/24"], &[]);
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
}
