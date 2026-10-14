/// `forward lookup = no` does NOT switch off the PTR forward-confirmation: a
/// reverse-DNS name whose A/AAAA records exclude the peer is a spoofed PTR
/// and must not satisfy a hostname `hosts allow` rule.
///
/// upstream: clientname.c:147-153 `client_name()` calls `check_name()`
/// unconditionally after a successful `getnameinfo`; `forward lookup` only
/// feeds access.c:60 `allow_forward_dns`, which gates resolving the
/// CONFIG token, never the peer's own name.
#[test]
fn module_peer_hostname_forward_lookup_off_still_confirms_the_ptr() {
    clear_test_hostname_overrides();
    let module = ModuleDefinition {
        forward_lookup: false,
        ..module_with_host_patterns(&["trusted.example.com"], &[])
    };
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6));
    // The attacker controls the PTR for its own address but not the forward
    // zone of the name it claims.
    set_test_hostname_override(peer, Some("trusted.example.com"));
    set_test_forward_override(
        "trusted.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2))],
    );
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert_eq!(
        resolved, None,
        "an unconfirmed PTR name must be discarded even with forward lookup = no"
    );
    assert!(!module.permits(peer, PeerHost::new(resolved, true)));
    clear_test_hostname_overrides();
}
