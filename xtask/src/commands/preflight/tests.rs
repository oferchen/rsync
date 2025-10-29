use super::*;
use crate::error::TaskError;
use std::ffi::OsString;

#[test]
fn parse_args_accepts_default_configuration() {
    let options = parse_args(std::iter::empty()).expect("parse succeeds");
    assert_eq!(options, PreflightOptions);
}

#[test]
fn parse_args_reports_help_request() {
    let error = parse_args([OsString::from("--help")]).unwrap_err();
    assert!(matches!(error, TaskError::Help(message) if message == usage()));
}

#[test]
fn parse_args_rejects_unknown_argument() {
    let error = parse_args([OsString::from("--unknown")]).unwrap_err();
    assert!(matches!(error, TaskError::Usage(message) if message.contains("preflight")));
}
