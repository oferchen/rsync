#[test]
fn module_definition_ipv4_allow_short_circuits_deny() {
    // upstream: access.c:277-279 - "If we match an allow-list item, we
    // always allow access." A peer that matches any entry in the allow
    // list is admitted before the deny list is consulted, even when an
    // overlapping deny rule would otherwise match.
    let module = module_with_host_patterns(&["192.168.0.0/16"], &["192.168.1.0/24"]);
    // In the allowed subnet and outside the deny subnet - permitted by allow.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), None));
    // In both the allowed and denied subnets - allow short-circuits the
    // deny rule, matching upstream.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)), None));
    // Outside both the allowed and denied subnets - admitted because
    // access.c:287 only refuses on a deny-list match; otherwise
    // access.c:291 falls through to "Allow all other access". The
    // allow-list non-match short-circuits to refuse only when the deny
    // list is empty (access.c:281-282).
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
}
