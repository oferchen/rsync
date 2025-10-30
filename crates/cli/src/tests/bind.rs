use super::common::*;
use super::*;

#[test]
fn bind_address_argument_accepts_ipv4_literal() {
    let parsed = parse_bind_address_argument(OsStr::new("192.0.2.1")).expect("parse bind address");
    let expected = "192.0.2.1".parse::<IpAddr>().expect("ip literal");
    assert_eq!(parsed.socket().ip(), expected);
    assert_eq!(parsed.raw(), OsStr::new("192.0.2.1"));
}

#[test]
fn bind_address_argument_rejects_empty_value() {
    let error = parse_bind_address_argument(OsStr::new(" ")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("--address requires a non-empty value")
    );
}
