/// A `hosts allow = @trusted` rule admits a client whose resolved hostname is a
/// member of the `trusted` netgroup. The `@name` token routes to the netgroup
/// membership check rather than literal hostname matching.
///
/// WHY: operators use netgroups to centrally manage the set of trusted hosts
/// (add a host to the netgroup once, and every module honoring `@trusted`
/// admits it) instead of enumerating hostnames per module. Membership is
/// injected via the `module_state` netgroup seam so the test is deterministic
/// without a real netgroup database.
///
/// upstream: access.c:41-42 `match_hostname` - `innetgr(tok + 1, host, NULL,
/// NULL)` tests the client hostname for membership in the `@`-prefixed group.
#[test]
fn module_hostname_allow_netgroup_admits_member() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["@trusted"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20));
    set_test_netgroup_members("trusted", &["client.example.com"]);

    assert!(
        module.permits(peer, Some("client.example.com")),
        "a client whose hostname is a member of the @netgroup allow rule must be admitted"
    );

    clear_test_hostname_overrides();
}
