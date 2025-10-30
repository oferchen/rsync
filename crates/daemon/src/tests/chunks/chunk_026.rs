#[test]
fn module_definition_hostname_suffix_matches() {
    let module = module_with_host_patterns(&[".example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("node.example.com")));
    assert!(module.permits(peer, Some("example.com")));
    assert!(!module.permits(peer, Some("example.net")));
    assert!(!module.permits(peer, Some("sampleexample.com")));
}

