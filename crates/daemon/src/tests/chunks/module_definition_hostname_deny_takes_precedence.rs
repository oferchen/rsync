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
    assert!(module.permits(peer, Some("bad.example.com")));
    assert!(module.permits(peer, Some("good.example.com")));
}

/// Hostname deny rules engage when the peer matches no allow pattern.
/// This pairs with the wildcard-allow case above: removing the wildcard
/// allows the deny list to gate access by hostname.
#[test]
fn module_definition_hostname_deny_applies_without_allow_match() {
    let module = module_with_host_patterns(&[], &["bad.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(!module.permits(peer, Some("bad.example.com")));
    assert!(module.permits(peer, Some("good.example.com")));
}

