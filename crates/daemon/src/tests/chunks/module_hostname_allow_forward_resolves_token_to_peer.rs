/// A `hosts allow` hostname token whose forward (A/AAAA) resolution includes
/// the connecting address admits the peer, even when the peer has no
/// reverse-DNS name. This is the forward-DNS branch of upstream access.c:
/// the admin-configured hostname is resolved and the connecting IP is matched
/// against the returned records, independent of reverse DNS.
///
/// WHY: without this branch a rule like `hosts allow = trusted.example.com`
/// would only work when the peer's PTR record happened to resolve to the same
/// name; upstream instead forward-resolves the token so the rule matches by
/// address. Verifying with a None reverse hostname proves the match comes from
/// forward resolution (upstream's UNDETERMINED-host case), not reverse lookup.
///
/// upstream: access.c:56-68 `match_hostname` - forward-DNS on the token and
/// compare each resolved address to the connecting address.
#[test]
fn module_hostname_allow_forward_resolves_token_to_peer() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

    // The peer has no PTR record, so reverse resolution yields None; the only
    // way to match the allow rule is by forward-resolving its hostname token.
    set_test_forward_override("trusted.example.com", &[peer]);

    assert!(
        module.permits(peer, None),
        "a hostname allow token forward-resolving to the peer must admit it"
    );

    clear_test_hostname_overrides();
}
