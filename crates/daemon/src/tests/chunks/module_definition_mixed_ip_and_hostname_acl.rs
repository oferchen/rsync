#[test]
fn module_definition_mixed_ip_and_hostname_acl() {
    // Allow a CIDR range and a hostname pattern; deny a specific IP.
    let module = module_with_host_patterns(
        &["192.168.0.0/16", "*.trusted.org"],
        &["192.168.1.99"],
    );
    // IP in allowed range, not in deny list.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), None));
    // IP in allowed range but also in deny list.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 99)), None));
    // IP outside allowed range but hostname matches allow pattern.
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)),
        Some("build.trusted.org"),
    ));
    // IP outside allowed range and hostname does not match.
    assert!(!module.permits(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)),
        Some("build.untrusted.org"),
    ));
}
