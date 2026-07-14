/// A `hosts deny` hostname token that forward-resolves to the connecting
/// address rejects the peer, even when the peer's reverse-DNS name does not
/// match the token. Forward resolution applies to the deny list just as it
/// does to the allow list.
///
/// WHY: upstream evaluates every token through the same
/// `match_hostname || match_address` predicate for both lists. A deny rule
/// naming a host must block that host by address, not only when the peer's PTR
/// happens to match the name. The peer here carries a resolved (Some) reverse
/// name that does NOT match the deny token, so the block can only come from
/// forward-resolving the token to the peer's address.
///
/// upstream: access.c:254 `match_hostname(...) || match_address(...)` is the
/// shared per-token predicate; access.c:287 applies it to the deny list.
#[test]
fn module_hostname_deny_forward_resolves_token_to_peer() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&[], &["blocked.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 14));
    // A resolved reverse name that does not match the deny token; this keeps
    // the fail-closed guard (which fires only on an unresolved hostname)
    // inactive, isolating the forward-resolution path.
    let reverse = Some("workstation.example.net");

    set_test_forward_override("blocked.example.com", &[peer]);
    assert!(
        !module.permits(peer, reverse),
        "a deny token forward-resolving to the peer must reject it"
    );

    // Positive control: when the deny token resolves elsewhere, the peer whose
    // reverse name matches no rule is permitted.
    set_test_forward_override(
        "blocked.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))],
    );
    assert!(
        module.permits(peer, reverse),
        "a deny token resolving to a different address must not block the peer"
    );

    clear_test_hostname_overrides();
}
