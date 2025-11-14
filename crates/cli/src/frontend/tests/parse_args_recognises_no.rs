use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_no_super_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-super"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.super_mode, Some(false));
}

#[test]
fn parse_args_recognises_no_verbose_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--no-verbose"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.verbosity, 0);
}

#[test]
fn parse_args_no_verbose_respects_following_verbose_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-verbose"),
        OsString::from("-vv"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.verbosity, 2);
}

#[test]
fn parse_args_recognises_no_delay_updates_flag() {
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

#[test]
fn parse_args_recognises_no_human_readable_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-human-readable"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Disabled));
}

#[test]
fn parse_args_recognises_no_bwlimit_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-bwlimit"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(matches!(parsed.bwlimit, Some(BandwidthArgument::Disabled)));
}

#[test]
fn parse_args_recognises_no_motd_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-motd"),
        OsString::from("rsync://example/"),
    ])
    .expect("parse");

    assert!(parsed.no_motd);
}
