#[test]
fn module_peer_hostname_skips_lookup_when_disabled() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    set_test_hostname_override(peer, Some("trusted.example.com"));
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, false);
    assert!(resolved.is_none());
    assert!(!module.permits(peer, resolved));
    clear_test_hostname_overrides();
}

