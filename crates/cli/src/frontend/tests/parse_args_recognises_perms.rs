use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_perms_and_times_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--perms"),
        OsString::from("--times"),
        OsString::from("--omit-dir-times"),
        OsString::from("--omit-link-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.perms, Some(true));
    assert_eq!(parsed.times, Some(true));
    assert_eq!(parsed.omit_dir_times, Some(true));
    assert_eq!(parsed.omit_link_times, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-perms"),
        OsString::from("--no-times"),
        OsString::from("--no-omit-dir-times"),
        OsString::from("--no-omit-link-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.perms, Some(false));
    assert_eq!(parsed.times, Some(false));
    assert_eq!(parsed.omit_dir_times, Some(false));
    assert_eq!(parsed.omit_link_times, Some(false));
}

#[test]
fn parse_args_prefers_last_perms_toggle() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--perms"),
        OsString::from("--no-perms"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse conflicting perms");

    assert_eq!(parsed.perms, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-perms"),
        OsString::from("--perms"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse overriding perms");

    assert_eq!(parsed.perms, Some(true));
}

#[test]
fn parse_args_recognises_short_capital_o_as_omit_dir_times() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-O"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse -O");

    assert_eq!(parsed.omit_dir_times, Some(true));
}

#[test]
fn parse_args_short_o_combined_with_times_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-tO"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse -tO");

    assert_eq!(parsed.times, Some(true));
    assert_eq!(parsed.omit_dir_times, Some(true));
}

#[test]
fn parse_args_no_omit_dir_times_overrides_short_o() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-O"),
        OsString::from("--no-omit-dir-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse -O --no-omit-dir-times");

    assert_eq!(parsed.omit_dir_times, Some(false));
}
