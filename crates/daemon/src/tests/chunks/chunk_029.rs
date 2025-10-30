#[test]
fn module_definition_hostname_wildcard_handles_multiple_asterisks() {
    let module = module_with_host_patterns(&["*build*node*.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("fastbuild-node1.example.com")));
    assert!(module.permits(peer, Some("build-node.example.com")));
    assert!(!module.permits(peer, Some("build.example.org")));
}

