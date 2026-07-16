/// A `hosts deny = @blocked` rule blocks a client whose resolved hostname is a
/// member of the `blocked` netgroup, while a non-member is admitted.
///
/// WHY: netgroups are equally useful for centrally maintained deny lists; a
/// `@netgroup` deny token must route through the same membership check and
/// block members. Verifying a non-member is still admitted proves the deny is
/// membership-scoped, not a blanket refusal.
///
/// upstream: access.c:41-42 via `access_match` over the deny list - a matching
/// `@netgroup` deny token returns 0 (access denied).
#[test]
fn module_hostname_netgroup_deny_blocks_member() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&[], &["@blocked"]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 22));
    set_test_netgroup_members("blocked", &["bad.example.com"]);

    assert!(
        !module.permits(peer, Some("bad.example.com")),
        "a member of the @netgroup deny rule must be blocked"
    );
    assert!(
        module.permits(peer, Some("good.example.com")),
        "a non-member must not be caught by the @netgroup deny rule"
    );

    clear_test_hostname_overrides();
}
