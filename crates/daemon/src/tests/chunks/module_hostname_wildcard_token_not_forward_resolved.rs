/// A wildcarded `hosts allow` token (containing `*`/`?`) is never
/// forward-resolved; it matches only via the reverse-DNS name pattern.
///
/// WHY: upstream restricts forward resolution to simple hostnames, skipping any
/// token that contains an address or wildcard metacharacter (`:` `/` `*` `?`
/// `[`). A wildcard entry is not a resolvable name, so it must not be handed to
/// the resolver. This guards the token-classification gate that mirrors
/// upstream's `strcspn(tok, ":/*?[")` check.
///
/// upstream: access.c:52-54 `match_hostname` - "Fail quietly if tok is an
/// address or wildcarded entry, not a simple hostname."
#[test]
fn module_hostname_wildcard_token_not_forward_resolved() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["*.trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 15));

    // Even with a forward override on the raw wildcard token, the wildcard is
    // not eligible for forward resolution, so a peer with no reverse name is
    // refused.
    set_test_forward_override("*.trusted.example.com", &[peer]);
    assert!(
        !module.permits(peer, None),
        "a wildcarded token must not be forward-resolved"
    );

    // The wildcard still matches a reverse-DNS name the normal way.
    assert!(
        module.permits(peer, Some("node.trusted.example.com")),
        "a wildcarded token must still match a reverse-DNS name"
    );

    clear_test_hostname_overrides();
}
