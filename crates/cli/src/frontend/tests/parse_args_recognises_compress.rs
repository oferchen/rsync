use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_compress_level_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-level=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.compress);
    assert_eq!(parsed.compress_level, Some(OsString::from("5")));
}

#[test]
fn parse_args_recognises_compress_threads_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--compress-threads=4"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_threads, Some(OsString::from("4")));
}

#[test]
fn parse_args_compress_threads_default_is_none() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_threads, None);
}

#[test]
fn parse_args_compress_threads_accepts_zt_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--zt=8"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.compress_threads, Some(OsString::from("8")));
}
