#[test]
fn module_definition_hostname_wildcard_collapses_consecutive_asterisks() {
    let module = module_with_host_patterns(&["**.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, PeerHost::new(Some("node.example.com"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("node.example.org"), true)));
}
