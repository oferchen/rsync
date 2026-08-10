use super::common::*;
use super::*;

#[test]
fn parse_args_rejects_invalid_checksum_choice() {
    let error = match parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum-choice=invalid"),
    ]) {
        Ok(_) => panic!("parse should fail"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(
        error
            .to_string()
            .contains("invalid --checksum-choice value 'invalid'")
    );
}
