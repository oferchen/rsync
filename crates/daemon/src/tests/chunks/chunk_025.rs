#[test]
fn module_definition_hostname_allow_matches_exact() {
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("trusted.example.com")));
    assert!(!module.permits(peer, Some("other.example.com")));
    assert!(!module.permits(peer, None));
}

