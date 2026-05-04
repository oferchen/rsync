use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_human_readable_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn parse_args_recognises_human_readable_level_two() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable=2"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}
