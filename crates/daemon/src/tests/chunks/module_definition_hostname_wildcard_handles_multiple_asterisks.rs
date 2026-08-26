#[test]
fn module_definition_hostname_wildcard_handles_multiple_asterisks() {
    let module = module_with_host_patterns(&["*build*node*.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(
        peer,
        PeerHost::new(Some("fastbuild-node1.example.com"), true)
    ));
    assert!(module.permits(peer, PeerHost::new(Some("build-node.example.com"), true)));
    assert!(!module.permits(peer, PeerHost::new(Some("build.example.org"), true)));
}
