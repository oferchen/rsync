use super::common::*;
use super::*;

#[test]
fn parse_args_resets_delay_updates_with_no_delay_updates() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--delay-updates"),
        OsString::from("--no-delay-updates"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.delay_updates);
}
