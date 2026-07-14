/// When a `hosts allow` hostname token cannot be forward-resolved (the name
/// does not resolve), the peer is not admitted. Resolution failure fails
/// closed rather than open.
///
/// WHY: a DNS outage or a typo'd hostname must never grant access. Upstream
/// treats a NULL `gethostbyname` result as no match; oc-rsync's shared
/// forward-resolution seam returns an empty address set on failure, so the
/// allow rule contributes nothing and an allow-list-only module refuses the
/// non-matching peer.
///
/// upstream: access.c:57-58 `match_hostname` - `if (!(hp = gethostbyname(tok)))
/// return 0;` fails closed on resolution error.
#[test]
fn module_hostname_allow_forward_resolve_failure_fails_closed() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12));

    // No forward override is registered, so the token "does not resolve" and
    // the resolver returns an empty address set.
    assert!(
        !module.permits(peer, None),
        "an unresolvable hostname allow token must fail closed (deny)"
    );

    clear_test_hostname_overrides();
}
