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
