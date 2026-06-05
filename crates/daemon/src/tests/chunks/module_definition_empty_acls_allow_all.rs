#[test]
fn module_definition_empty_acls_allow_all() {
    // upstream: access.c - when neither hosts_allow nor hosts_deny is set,
    // all connections are permitted.
    let module = module_with_host_patterns(&[], &[]);
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), None));
    assert!(module.permits(IpAddr::V6(Ipv6Addr::LOCALHOST), None));
}
