use super::prelude::*;


#[test]
fn resolve_daemon_addresses_filters_ipv4_mode() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv4).expect("ipv4 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv4));
}


#[test]
fn resolve_daemon_addresses_rejects_missing_ipv6_addresses() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let error = resolve_daemon_addresses(&address, AddressMode::Ipv6)
        .expect_err("IPv6 filtering should fail for IPv4-only host");

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("does not have IPv6 addresses"));
}


#[test]
fn resolve_daemon_addresses_filters_ipv6_mode() {
    let address = DaemonAddress::new(String::from("::1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv6).expect("ipv6 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv6));
}

