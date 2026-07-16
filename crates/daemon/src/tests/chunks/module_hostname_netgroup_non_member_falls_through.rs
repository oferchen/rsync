/// A `hosts allow = @trusted` rule does not admit a client that is not a member
/// of the netgroup, so with no other allow rule the connection falls through to
/// deny-by-default. This same path covers the musl/Windows no-op, where
/// `innetgr` is unavailable and a `@netgroup` token can never match.
///
/// WHY: an unresolvable or unsupported netgroup must yield a clean non-match,
/// never an error or panic, exactly as an upstream build compiled without
/// `HAVE_INNETGR`. Declaring no members reproduces both a genuine non-member and
/// the no-op platform, pinning that `@netgroup` simply fails to match rather
/// than crashing or spuriously admitting the peer.
///
/// upstream: access.c:40-43 - the `innetgr` branch is compiled only under
/// `HAVE_INNETGR`; without it a `@name` token never matches.
#[test]
fn module_hostname_netgroup_non_member_falls_through() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["@trusted"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 21));
    // No members declared: mirrors both a genuine non-member and the
    // musl/Windows no-op resolver that always reports no membership.

    assert!(
        !module.permits(peer, Some("stranger.example.com")),
        "a non-member (or the no-op netgroup platform) must not satisfy an @netgroup allow rule"
    );

    clear_test_hostname_overrides();
}
