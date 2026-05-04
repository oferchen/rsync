use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_numeric_ids_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--numeric-ids"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.numeric_ids, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-numeric-ids"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.numeric_ids, Some(false));
}
