/// A `hosts allow` hostname token that forward-resolves to a DIFFERENT address
/// than the peer does not admit the peer. With an allow list and no matching
/// entry (and no deny list), access is refused.
///
/// WHY: forward resolution must match by address, not merely succeed. A token
/// resolving to some other host must not grant access to an unrelated peer -
/// otherwise the allow list would be meaningless. Mirrors upstream's per-record
/// `strcmp(addr, ...)` comparison.
///
/// upstream: access.c:60-61 `match_hostname` - each resolved address is
/// compared to the connecting address; a mismatch contributes no match.
#[test]
fn module_hostname_allow_forward_resolve_non_matching_denies() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));

    // The token resolves, but to a different address than the peer.
    set_test_forward_override(
        "trusted.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))],
    );

    assert!(
        !module.permits(peer, None),
        "a hostname allow token resolving to a different address must not \
         admit an unrelated peer"
    );

    clear_test_hostname_overrides();
}
