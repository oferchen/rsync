/// Upstream rsync admits a peer on the first allow-list match without
/// consulting the deny list (`access.c::allow_access`, "If we match an
/// allow-list item, we always allow access."). A wildcard `hosts allow = *`
/// therefore short-circuits any subsequent hostname-pattern deny rule.
/// To make a hostname deny rule effective the operator must omit the
/// catch-all allow (or list only the trusted hosts explicitly).
#[test]
fn module_definition_hostname_deny_short_circuited_by_wildcard_allow() {
    let module = module_with_host_patterns(&["*"], &["bad.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, PeerHost::new(Some("bad.example.com"), true)));
    assert!(module.permits(peer, PeerHost::new(Some("good.example.com"), true)));
}

/// Hostname deny rules engage when the peer matches no allow pattern.
/// This pairs with the wildcard-allow case above: removing the wildcard
/// allows the deny list to gate access by hostname.
///
/// The deny token is given a forward resolution deliberately. Upstream returns
/// the caller's `deny` flag when a token will not resolve (access.c:57-63), so
/// an unresolvable deny token blocks EVERY peer - and the second assertion
/// below would then be asserting the absence of a security property rather
/// than the presence of a matching rule. Resolving the token elsewhere isolates
/// the reverse-name match, which is what this test is about.
#[test]
fn module_definition_hostname_deny_applies_without_allow_match() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&[], &["bad.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    set_test_forward_override(
        "bad.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))],
    );

    assert!(!module.permits(peer, PeerHost::new(Some("bad.example.com"), true)));
    assert!(module.permits(peer, PeerHost::new(Some("good.example.com"), true)));

    clear_test_hostname_overrides();
}
