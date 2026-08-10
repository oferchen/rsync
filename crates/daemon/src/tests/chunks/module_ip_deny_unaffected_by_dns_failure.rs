/// Regression test paired with `module_hostname_deny_fails_closed_when_dns_unresolved`.
///
/// The GHSA-rjfm-3w2m-jf4f fix changes deny-rule evaluation only when a
/// hostname-based pattern is present. IP-only deny rules must continue to
/// match (or not) purely on the peer IP and must NOT be affected by a
/// failed reverse DNS lookup. This pins the scope of the fail-closed
/// behaviour to hostname-pattern rules.
#[test]
fn module_ip_deny_unaffected_by_dns_failure() {
    clear_test_hostname_overrides();

    // IP-only deny pattern, no hostname patterns.
    let module = module_with_host_patterns(&[], &["192.0.2.0/24"]);

    let denied_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 50));
    let allowed_peer = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

    // Both peers have failed reverse DNS.
    set_test_hostname_override(denied_peer, None);
    set_test_hostname_override(allowed_peer, None);

    // Denied peer is still denied (IP rule matches regardless of hostname).
    assert!(
        !module.permits(denied_peer, None),
        "IP-pattern deny rule must continue to match the peer IP even when \
         reverse DNS fails"
    );

    // Allowed peer is still allowed. The fail-closed guard introduced by
    // GHSA-rjfm-3w2m-jf4f only activates when at least one deny rule is
    // hostname-based; pure IP-rule modules retain their original semantics.
    assert!(
        module.permits(allowed_peer, None),
        "IP-only deny rule must not fail closed on unresolved DNS for peers \
         outside the denied range"
    );

    clear_test_hostname_overrides();
}
