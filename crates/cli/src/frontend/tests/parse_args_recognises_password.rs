use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_password_file() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-file"),
        OsString::from("secret.txt"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.password_file, Some(OsString::from("secret.txt")));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-file=secrets.d"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.password_file, Some(OsString::from("secrets.d")));
}

#[test]
fn parse_args_recognises_password_command() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-command"),
        OsString::from("pass show rsync/server"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.password_command,
        Some(OsString::from("pass show rsync/server"))
    );

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--password-command=echo secret"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.password_command,
        Some(OsString::from("echo secret"))
    );
}
