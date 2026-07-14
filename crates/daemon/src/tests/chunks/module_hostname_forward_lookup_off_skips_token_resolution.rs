/// `forward lookup = no` disables forward-DNS resolution of `hosts allow`/
/// `hosts deny` hostname tokens. A token that would forward-resolve to the
/// peer does NOT admit it when the peer lacks a matching reverse-DNS name.
///
/// WHY: this pins the forward-resolution branch to the `forward lookup`
/// parameter exactly as upstream gates it on `allow_forward_dns`. The same
/// override that admits the peer with `forward lookup = yes` (the module
/// default) must be ignored once forward lookup is turned off - proving the
/// branch is controlled by the parameter, not always-on.
///
/// upstream: access.c:49 `if (!allow_forward_dns) return 0;` short-circuits the
/// forward lookup when the `forward lookup` parameter is disabled.
#[test]
fn module_hostname_forward_lookup_off_skips_token_resolution() {
    clear_test_hostname_overrides();
    let module = ModuleDefinition {
        forward_lookup: false,
        ..module_with_host_patterns(&["trusted.example.com"], &[])
    };
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 13));

    // The token would forward-resolve to the peer, but forward lookup is off.
    set_test_forward_override("trusted.example.com", &[peer]);

    assert!(
        !module.permits(peer, None),
        "forward lookup = no must skip forward resolution of hostname tokens"
    );

    // Positive control: the same setup with forward lookup enabled admits the
    // peer, confirming the deny above is due to the disabled parameter alone.
    let enabled = module_with_host_patterns(&["trusted.example.com"], &[]);
    assert!(
        enabled.permits(peer, None),
        "forward lookup = yes (default) must admit via the same token"
    );

    clear_test_hostname_overrides();
}
