use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_log_file_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        OsString::from("/var/log/rsync.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.log_file, Some(OsString::from("/var/log/rsync.log")));
}

#[test]
fn parse_args_recognises_log_file_format_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        OsString::from("/var/log/rsync.log"),
        OsString::from("--log-file-format"),
        OsString::from("%f %l"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.log_file_format, Some(OsString::from("%f %l")));
}

#[test]
fn parse_args_recognises_log_file_format_equals_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        OsString::from("/var/log/rsync.log"),
        OsString::from("--log-file-format=%l %f"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.log_file_format, Some(OsString::from("%l %f")));
}
