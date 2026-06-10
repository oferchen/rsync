/// Regression test for GHSA-rjfm-3w2m-jf4f (CVE-2026-43617): a hostname-
/// based `hosts deny` rule must reject a connection when reverse DNS
/// resolution fails, even though the matcher cannot prove the peer's name
/// matches the pattern.
///
/// Upstream rsync 3.4.3 fixes this by performing reverse DNS before
/// `daemon chroot` and caching the result, so `client_name()` returns a
/// real hostname at ACL evaluation time. oc-rsync's daemon uses a
/// thread-per-connection model where `daemon chroot` is applied process-
/// wide at startup, so per-peer DNS post-chroot can silently fail when the
/// chroot lacks NSS configuration. To close the bypass without relying on
/// the chroot containing NSS files, the matcher fails closed when the
/// hostname is unresolvable and any deny rule is hostname-based.
///
/// Scenario A from upstream's `testsuite/daemon-chroot-acl.test`: global
/// `reverse lookup = yes`, `hosts deny = <hostname>`, peer reverse DNS
/// fails (returns None). Pre-fix: deny pattern cannot match -> permits
/// returns true -> ACL bypass. Post-fix: hostname-pattern deny rule
/// present + unresolved hostname -> permits returns false.
///
/// upstream: clientserver.c (3.4.3 commit c38f20c5) "clientserver: fix
/// hostname ACL bypass when using daemon chroot".
#[test]
fn module_hostname_deny_fails_closed_when_dns_unresolved() {
    clear_test_hostname_overrides();

    // Deny rule references a hostname pattern; no allow rule, so a
    // resolvable peer outside the pattern would normally be permitted.
    let module = module_with_host_patterns(&[], &["*.bad.example"]);

    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));

    // Simulate the post-chroot DNS failure: reverse lookup yields None.
    set_test_hostname_override(peer, None);

    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert!(
        resolved.is_none(),
        "simulated post-chroot DNS failure must produce None hostname"
    );

    // The critical security property: an unresolvable hostname must not
    // bypass a hostname-pattern deny rule. The matcher fails closed so an
    // attacker who controls their PTR record (or simply blackholes reverse
    // DNS) cannot dodge the deny list.
    assert!(
        !module.permits(peer, resolved),
        "hostname-pattern deny rule must reject peers whose reverse DNS \
         fails (GHSA-rjfm-3w2m-jf4f)"
    );

    clear_test_hostname_overrides();
}
