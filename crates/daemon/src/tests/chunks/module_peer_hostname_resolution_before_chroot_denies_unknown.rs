/// Regression test for CVE-2026-43617: hostname-based ACL deny rules must
/// reject connections when reverse DNS resolution fails, regardless of
/// chroot configuration.
///
/// The upstream C vulnerability occurred because reverse DNS was performed
/// after entering the daemon chroot. If the chroot lacked resolver files
/// (/etc/resolv.conf, /etc/nsswitch.conf), the lookup failed silently and
/// the hostname was set to "UNKNOWN", causing hostname-based deny rules to
/// fail open.
///
/// oc-rsync resolves hostnames in the module access phase (request.rs /
/// listing.rs) before chroot is applied in the transfer phase (transfer.rs).
/// This test pins down that when DNS resolution returns None (simulating a
/// failed lookup), a module restricted to hostname-only allow rules denies
/// access - the "fail closed" property that the C version lacked under
/// chroot.
///
/// upstream: clientserver.c - rsync 3.4.3 moved reverse DNS before chroot.
#[test]
fn module_peer_hostname_resolution_before_chroot_denies_unknown() {
    clear_test_hostname_overrides();

    // Module with use_chroot = true and hostname-only allow rule.
    // Only connections from "trusted.internal" should be permitted.
    let module = ModuleDefinition {
        use_chroot: true,
        ..module_with_host_patterns(&["trusted.internal"], &[])
    };

    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 42));

    // Simulate DNS resolution failure (as would happen inside a chroot
    // lacking resolver files): set the override to None so
    // module_peer_hostname returns None.
    set_test_hostname_override(peer, None);

    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);

    // Resolution failed - hostname is None.
    assert!(
        resolved.is_none(),
        "simulated DNS failure must produce None hostname"
    );

    // The critical security property: with no resolved hostname, the
    // hostname-only allow rule cannot match, so access must be denied.
    assert!(
        !module.permits(peer, resolved),
        "module must deny access when hostname resolution fails and only \
         hostname-based allow rules are configured (CVE-2026-43617)"
    );

    clear_test_hostname_overrides();
}
