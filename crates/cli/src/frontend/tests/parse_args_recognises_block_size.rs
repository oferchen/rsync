use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_block_size_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--block-size=16384"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.block_size, Some(OsString::from("16384")));
}

#[test]
fn parse_args_rejects_non_numeric_block_size() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--block-size=abc"),
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
            .contains("invalid --block-size value 'abc'")
    );
}

#[test]
fn parse_args_rejects_negative_block_size() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--block-size=-1"),
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
            .contains("invalid --block-size value '-1'")
    );
}
