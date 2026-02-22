use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_modify_window() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.modify_window, Some(OsString::from("5")));
}

#[test]
fn parse_args_recognises_modify_window_zero() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=0"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.modify_window, Some(OsString::from("0")));
}

#[test]
fn parse_args_rejects_negative_modify_window() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=-1"),
        OsString::from("source"),
        OsString::from("dest"),
    ]) {
        Ok(_) => panic!("parse should fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("invalid --modify-window value '-1'")
    );
}

#[test]
fn parse_args_rejects_non_numeric_modify_window() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--modify-window=fat"),
        OsString::from("source"),
        OsString::from("dest"),
    ]) {
        Ok(_) => panic!("parse should fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("invalid --modify-window value 'fat'")
    );
}
