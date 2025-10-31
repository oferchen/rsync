use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_stats_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.stats);
}
