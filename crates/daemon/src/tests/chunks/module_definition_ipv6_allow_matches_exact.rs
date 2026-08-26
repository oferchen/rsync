#[test]
fn module_definition_ipv6_allow_matches_exact() {
    let module = module_with_host_patterns(&["::1"], &[]);
    assert!(module.permits(IpAddr::V6(Ipv6Addr::LOCALHOST), PeerHost::new(None, true)));
    assert!(!module.permits(
        IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 2)),
        PeerHost::new(None, true)
    ));
    // IPv4 peer does not match an IPv6 allow rule.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::LOCALHOST), PeerHost::new(None, true)));
}
