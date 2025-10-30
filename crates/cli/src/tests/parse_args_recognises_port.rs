use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_port_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--port=10873"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.daemon_port, Some(10873));
}
