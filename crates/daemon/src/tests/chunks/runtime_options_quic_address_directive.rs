// QUIC listener addresses (oc extension). The `quic address` global directive
// lists the literal IPv4/IPv6 addresses the QUIC listener binds, independent of
// the TCP `address`. Unset, the listener follows the TCP addresses (dual-stack
// by default). A hostname, an invalid address or a duplicate is a config error
// naming the directive, so the listener never binds somewhere the operator did
// not write. These configs set no certificate, which only listener startup
// rejects, so they parse on Unix.

fn parse_quic_address_config(body: &str) -> Result<RuntimeOptions, DaemonError> {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, body).expect("write config");
    RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
}

fn quic_address_of(body: &str) -> Option<Vec<IpAddr>> {
    parse_quic_address_config(body)
        .expect("parse config")
        .quic_address()
        .map(<[IpAddr]>::to_vec)
}

fn quic_address_error(body: &str) -> String {
    parse_quic_address_config(body)
        .expect_err("the quic address value must be refused")
        .to_string()
}

#[test]
fn quic_address_unset_follows_tcp_addresses() {
    assert_eq!(quic_address_of("port = 873\n"), None);
}

#[test]
fn quic_address_single_ipv4() {
    assert_eq!(
        quic_address_of("quic address = 127.0.0.1\n"),
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    );
}

#[test]
fn quic_address_single_ipv6() {
    assert_eq!(
        quic_address_of("quic address = ::1\n"),
        Some(vec![IpAddr::V6(Ipv6Addr::LOCALHOST)])
    );
}

#[test]
fn quic_address_dual_stack_list_keeps_order() {
    assert_eq!(
        quic_address_of("quic address =  127.0.0.1   ::1 \n"),
        Some(vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        ])
    );
}

#[test]
fn quic_address_hostname_is_config_error() {
    let text = quic_address_error("quic address = localhost\n");
    assert!(
        text.contains("invalid 'quic address': 'localhost' is not an IPv4 or IPv6 address"),
        "got: {text}"
    );
}

#[test]
fn quic_address_bad_ip_is_config_error() {
    let text = quic_address_error("quic address = 127.0.0.1 10.0.0.256\n");
    assert!(
        text.contains("invalid 'quic address': '10.0.0.256' is not an IPv4 or IPv6 address"),
        "got: {text}"
    );
}

#[test]
fn quic_address_duplicate_is_config_error() {
    let text = quic_address_error("quic address = ::1 127.0.0.1 ::1\n");
    assert!(
        text.contains("invalid 'quic address': '::1' is listed more than once"),
        "got: {text}"
    );
}

#[test]
fn quic_address_empty_is_config_error() {
    let text = quic_address_error("quic address =\n");
    assert!(
        text.contains("invalid 'quic address': expected at least one IPv4 or IPv6 address"),
        "got: {text}"
    );
}

#[test]
fn quic_address_in_module_scope_is_config_error() {
    let text = quic_address_error("[data]\npath = /srv/data\nquic address = 127.0.0.1\n");
    assert!(
        text.contains("'quic address' is a global-only directive"),
        "got: {text}"
    );
}
