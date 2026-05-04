#[test]
fn module_peer_hostname_missing_resolution_denies_hostname_only_rules() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    if let Some(host) = resolved {
        assert_ne!(host, "trusted.example.com");
    }
    assert!(!module.permits(peer, resolved));
}

