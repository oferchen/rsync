#[test]
fn module_definition_wildcard_any_allows_all() {
    let module = module_with_host_patterns(&["*"], &[]);
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)),
        PeerHost::new(None, true)
    ));
    assert!(module.permits(IpAddr::V6(Ipv6Addr::LOCALHOST), PeerHost::new(None, true)));
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        PeerHost::new(Some("anything.example.com"), true)
    ));
}
