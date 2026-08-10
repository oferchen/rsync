#[test]
fn module_definition_hostname_wildcard_matches() {
    let module = module_with_host_patterns(&["build?.example.*"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("build1.example.net")));
    assert!(module.permits(peer, Some("builda.example.org")));
    assert!(!module.permits(peer, Some("build12.example.net")));
}

