/// `forward lookup = no` restores upstream's PTR-only behaviour: the
/// reverse-DNS name is trusted without forward-confirmation. This mirrors
/// upstream access.c where `allow_forward_dns` (from `lp_forward_lookup`)
/// being false skips the forward check entirely.
///
/// upstream: access.c:49 - `if (!allow_forward_dns) return 0;` short-circuits
/// the forward lookup when the `forward lookup` parameter is disabled.
#[test]
fn module_peer_hostname_forward_lookup_off_restores_ptr_only() {
    clear_test_hostname_overrides();
    let module = ModuleDefinition {
        forward_lookup: false,
        ..module_with_host_patterns(&["trusted.example.com"], &[])
    };
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6));

    // PTR points at the trusted name; the forward lookup does NOT include the
    // peer (would fail confirmation) - but with `forward lookup = no` the
    // confirmation is skipped and the PTR name is trusted as-is.
    set_test_hostname_override(peer, Some("trusted.example.com"));
    set_test_forward_override(
        "trusted.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2))],
    );

    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert_eq!(
        resolved,
        Some("trusted.example.com"),
        "forward lookup = no must trust the PTR name without confirmation"
    );
    assert!(module.permits(peer, resolved));

    clear_test_hostname_overrides();
}
