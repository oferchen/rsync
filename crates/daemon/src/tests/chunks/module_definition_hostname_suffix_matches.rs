#[test]
fn module_definition_hostname_suffix_matches() {
    let module = module_with_host_patterns(&[".example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, PeerHost::new(Some("node.example.com"), true)));
    assert!(module.permits(peer, PeerHost::new(Some("example.com"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("example.net"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("sampleexample.com"), true)));
}
