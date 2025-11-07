#[test]
fn module_definition_hostname_deny_takes_precedence() {
    let module = module_with_host_patterns(&["*"], &["bad.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(!module.permits(peer, Some("bad.example.com")));
    assert!(module.permits(peer, Some("good.example.com")));
}

