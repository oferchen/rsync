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

#[test]
fn parse_args_recognises_log_file_equals_syntax() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file=/tmp/rsync.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.log_file, Some(OsString::from("/tmp/rsync.log")));
}

#[test]
fn parse_args_log_file_defaults_to_none() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.log_file, None);
    assert_eq!(parsed.log_file_format, None);
}

#[test]
fn parse_args_log_file_format_without_log_file_is_accepted() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file-format=%n"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    // The format is stored even without --log-file; it has no effect without it.
    assert_eq!(parsed.log_file_format, Some(OsString::from("%n")));
    assert_eq!(parsed.log_file, None);
}

#[test]
fn parse_args_log_file_preserves_path_with_spaces() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--log-file"),
        OsString::from("/tmp/my logs/rsync.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(
        parsed.log_file,
        Some(OsString::from("/tmp/my logs/rsync.log"))
    );
}

#[test]
fn parse_args_log_file_combined_with_dry_run() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        OsString::from("--log-file=/tmp/rsync.log"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert!(parsed.dry_run);
    assert_eq!(parsed.log_file, Some(OsString::from("/tmp/rsync.log")));
}

#[test]
fn parse_args_log_file_combined_with_verbose() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--log-file=/tmp/rsync.log"),
        OsString::from("--log-file-format=%i %n"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse args");

    assert_eq!(parsed.verbosity, 1);
    assert_eq!(parsed.log_file, Some(OsString::from("/tmp/rsync.log")));
    assert_eq!(parsed.log_file_format, Some(OsString::from("%i %n")));
}
