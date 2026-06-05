#[test]
fn module_definition_ipv4_cidr_24_boundary() {
    let module = module_with_host_patterns(&["192.168.1.0/24"], &[]);
    // First and last address in /24.
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0)), None));
    assert!(module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 255)), None));
    // One address outside the /24 boundary on each side.
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 255)), None));
    assert!(!module.permits(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0)), None));
}
