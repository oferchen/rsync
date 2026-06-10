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
    // Outside the allowed subnet entirely - refused because the allow
    // list is non-empty and the peer matches nothing.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None));
}
