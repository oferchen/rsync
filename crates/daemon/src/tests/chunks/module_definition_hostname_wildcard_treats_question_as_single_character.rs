#[test]
fn module_definition_hostname_wildcard_treats_question_as_single_character() {
    let module = module_with_host_patterns(&["app??.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, PeerHost::new(Some("app12.example.com"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("app1.example.com"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("app123.example.com"), true)));
}

