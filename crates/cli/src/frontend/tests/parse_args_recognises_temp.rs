use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_temp_dir_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--temp-dir=.rsync-tmp"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new(".rsync-tmp")));
}

#[test]
fn parse_args_recognises_temp_dir_short_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-T"),
        OsString::from("/tmp/staging"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.temp_dir.as_deref(),
        Some(Path::new("/tmp/staging"))
    );
}

#[test]
fn parse_args_recognises_temp_dir_long_with_separate_value() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--temp-dir"),
        OsString::from("/var/tmp"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new("/var/tmp")));
}

#[test]
fn parse_args_recognises_temp_dir_with_relative_path() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--temp-dir=./staging"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.temp_dir.as_deref(), Some(Path::new("./staging")));
}

#[test]
fn parse_args_temp_dir_defaults_to_none() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(
        parsed.temp_dir.is_none(),
        "temp_dir should be None by default"
    );
}
