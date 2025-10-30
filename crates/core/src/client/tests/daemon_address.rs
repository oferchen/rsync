use super::prelude::*;


#[test]
fn daemon_address_accepts_ipv6_zone_identifier() {
    let address =
        DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("zone identifier accepted");
    assert_eq!(address.host(), "fe80::1%eth0");
    assert_eq!(address.port(), 873);

    let display = format!("{}", address.socket_addr_display());
    assert_eq!(display, "[fe80::1%eth0]:873");
}


#[test]
fn daemon_address_trims_host_whitespace() {
    let address =
        DaemonAddress::new("  example.com  ".to_string(), 873).expect("address trims host");
    assert_eq!(address.host(), "example.com");
    assert_eq!(address.port(), 873);
}

