#[test]
fn module_peer_hostname_uses_override() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    set_test_hostname_override(peer, Some("Trusted.Example.Com"));
    // forward lookup (module default true) confirms the PTR name resolves
    // back to the peer; a legitimate host does.
    set_test_forward_override("trusted.example.com", &[peer]);
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    // upstream client_name() never folds the PTR name, so `%h` and
    // RSYNC_HOST_NAME keep its case; `hosts allow` still matches because the
    // matcher folds the host like upstream iwildmatch (access.c:57).
    assert_eq!(resolved, Some("Trusted.Example.Com"));
    assert!(module.permits(peer, PeerHost::new(resolved, true)));
    clear_test_hostname_overrides();
}
