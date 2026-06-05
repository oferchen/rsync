#[test]
fn module_definition_only_deny_allows_non_matching() {
    // upstream: access.c - when only hosts_deny is set, anything not denied
    // is allowed.
    let module = module_with_host_patterns(&[], &["10.0.0.0/8"]);
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), None));
    assert!(module.permits(IpAddr::V6(Ipv6Addr::LOCALHOST), None));
}
