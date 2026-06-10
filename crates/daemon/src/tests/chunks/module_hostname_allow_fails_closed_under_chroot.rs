/// Regression test for GHSA-rjfm-3w2m-jf4f (CVE-2026-43617), Scenario B
/// from upstream's `testsuite/daemon-chroot-acl.test`: a hostname-based
/// `hosts allow` rule combined with `use chroot = yes` must refuse a
/// connection when reverse DNS fails post-chroot.
///
/// The fail-closed property here is structural rather than a guard added
/// by the GHSA patch: with `hosts allow` non-empty, `permits()` requires
/// at least one allow pattern to match the peer. A `Hostname(_)` pattern
/// returns `false` whenever the peer hostname is `None`, so an
/// unresolvable peer cannot satisfy a hostname-only allow list and is
/// refused. This test pins that property so future refactors of
/// `permits()` cannot accidentally introduce an allowed-by-default path
/// for the allow-side of the GHSA scenario.
///
/// Pairs with `module_hostname_deny_fails_closed_when_dns_unresolved`
/// (Scenario A, deny direction, closed by the explicit guard in
/// `permits()`).
///
/// upstream: clientserver.c - `allow_access()` evaluates `hosts allow`
/// before `hosts deny`; an empty allow match refuses the connection.
/// upstream: clientserver.c (3.4.3 commit c38f20c5) "clientserver: fix
/// hostname ACL bypass when using daemon chroot" - reverse DNS is
/// performed before chroot so `client_name()` returns a real hostname at
/// ACL evaluation time.
#[test]
fn module_hostname_allow_fails_closed_under_chroot() {
    clear_test_hostname_overrides();

    // Module mirrors upstream's `testsuite/daemon-chroot-acl.test`
    // Scenario B fixture: `use chroot = yes`, single hostname-pattern
    // `hosts allow` entry, no `hosts deny`.
    let module = ModuleDefinition {
        use_chroot: true,
        ..module_with_host_patterns(&["test-host.example"], &[])
    };

    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));

    // Simulate the post-chroot DNS failure: the chroot lacks
    // `/etc/resolv.conf` / NSS shared objects, so reverse DNS returns
    // `None`. Upstream rsync 3.4.3 sidesteps this by caching the lookup
    // before entering chroot; oc-rsync's module-level path resolves the
    // hostname in `module_access::request.rs::process_module_request()`
    // before `apply_module_privilege_restrictions()` enters the chroot,
    // and the matcher fails closed when resolution is impossible.
    set_test_hostname_override(peer, None);

    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert!(
        resolved.is_none(),
        "simulated post-chroot DNS failure must produce None hostname"
    );

    // The security property: an unresolved hostname must not satisfy a
    // hostname-only `hosts allow` list. The peer is refused even though
    // its IP is not explicitly denied.
    assert!(
        !module.permits(peer, resolved),
        "hostname-pattern `hosts allow` rule must refuse peers whose \
         reverse DNS fails under chroot (GHSA-rjfm-3w2m-jf4f Scenario B)"
    );

    // Sanity check: a resolvable peer matching the pattern is admitted,
    // so the refusal above is attributable to the DNS failure and not a
    // broken matcher.
    set_test_hostname_override(peer, Some("test-host.example"));
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert_eq!(
        resolved.as_deref(),
        Some("test-host.example"),
        "override must surface the configured hostname for the positive case"
    );
    assert!(
        module.permits(peer, resolved),
        "matching hostname must be admitted; the prior refusal is from \
         the DNS failure, not a pattern mismatch"
    );

    clear_test_hostname_overrides();
}
