/// A spoofed PTR record - one whose forward (A/AAAA) resolution does not
/// include the peer's address - must not satisfy a hostname `hosts allow`
/// rule. With `forward lookup = yes` (the module default), the reverse-DNS
/// name is forward-confirmed against the peer address; an unconfirmed name is
/// treated as no hostname, so the allow-rule fails closed.
///
/// upstream: clientname.c:416 `check_name` - a name whose forward lookup does
/// not match the peer address is replaced with "UNKNOWN"; access.c:49
/// `allow_forward_dns` gates this on the `forward lookup` parameter.
#[test]
fn module_peer_hostname_forward_confirm_rejects_spoofed_ptr() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));

    // Attacker controls the PTR record and points it at the trusted name.
    set_test_hostname_override(peer, Some("trusted.example.com"));
    // But the forward lookup of that name resolves to a DIFFERENT address,
    // so the confirmation fails.
    set_test_forward_override(
        "trusted.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))],
    );

    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert!(
        resolved.is_none(),
        "a PTR name that does not forward-resolve to the peer must be rejected"
    );
    assert!(
        !module.permits(peer, resolved),
        "spoofed PTR must not satisfy a hostname `hosts allow` rule"
    );

    // Positive control: when the forward lookup DOES include the peer, the
    // name is confirmed and the allow-rule matches.
    let mut cache = None;
    set_test_forward_override("trusted.example.com", &[peer]);
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert_eq!(resolved, Some("trusted.example.com"));
    assert!(module.permits(peer, resolved));

    clear_test_hostname_overrides();
}
