use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_delay_updates_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--delay-updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.delay_updates);
}
