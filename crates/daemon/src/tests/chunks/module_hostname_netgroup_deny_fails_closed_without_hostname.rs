/// A `hosts deny = @blocked` rule fails closed when the peer has no resolved
/// hostname: the connection is denied because the netgroup membership needed to
/// clear the deny rule cannot be evaluated.
///
/// WHY: like a hostname deny token, a `@netgroup` deny token is meaningless
/// without a resolved client hostname. Admitting a peer whose name is unknown
/// would let a client evade a deny rule simply by lacking a PTR record. The
/// fail-closed guard (GHSA-rjfm-3w2m-jf4f) must therefore treat `@netgroup`
/// deny rules as requiring a hostname, matching upstream's `if (!host || !*host)
/// return 0` bail-out before the membership test (access.c:37-38).
///
/// upstream: access.c:37-38 - `match_hostname` returns no-match when the host
/// is absent, before reaching the `innetgr` netgroup branch.
#[test]
fn module_hostname_netgroup_deny_fails_closed_without_hostname() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&[], &["@blocked"]);
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 23));

    assert!(
        !module.permits(peer, None),
        "an @netgroup deny rule must fail closed when the peer has no resolved hostname"
    );

    clear_test_hostname_overrides();
}
