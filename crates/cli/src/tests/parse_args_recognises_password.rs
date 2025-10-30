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
