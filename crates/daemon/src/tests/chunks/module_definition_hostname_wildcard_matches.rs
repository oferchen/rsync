#[test]
fn module_definition_hostname_wildcard_matches() {
    let module = module_with_host_patterns(&["build?.example.*"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, PeerHost::new(Some("build1.example.net"), true)));
    assert!(module.permits(peer, PeerHost::new(Some("builda.example.org"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("build12.example.net"), true)));
}
