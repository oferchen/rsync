#[test]
fn module_definition_mixed_ip_and_hostname_acl() {
    // Allow a CIDR range plus a hostname pattern; deny a specific IP.
    // upstream: access.c::allow_access - an allow-list match returns 1
    // immediately, so a peer admitted by the allow list bypasses the
    // deny list entirely. When the allow list does not match, the deny
    // list refuses only on a positive match.
    let module = module_with_host_patterns(
        &["192.168.0.0/16", "*.trusted.org"],
        &["192.168.1.99"],
    );
    // IP in allowed range, not in deny list - permitted by allow.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), None));
    // IP in allowed range and also in deny list - the allow match short-
    // circuits the deny check, matching upstream `allow_access`.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 99)), None));
    // IP outside allowed range but hostname matches allow pattern - allow.
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)),
        Some("build.trusted.org"),
    ));
    // IP outside allowed range, hostname not in allow, IP not in deny -
    // fall-through after a non-matching allow list with a non-empty deny
    // list admits the peer per upstream access.c:290.
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)),
        Some("build.untrusted.org"),
    ));
}
