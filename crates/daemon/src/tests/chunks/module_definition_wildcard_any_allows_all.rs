#[test]
fn module_definition_wildcard_any_allows_all() {
    let module = module_with_host_patterns(&["*"], &[]);
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), None));
    assert!(module.permits(IpAddr::V6(Ipv6Addr::LOCALHOST), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::LOCALHOST), Some("anything.example.com")));
}
