use super::common::*;
use super::*;

#[test]
fn timeout_argument_zero_disables_timeout() {
    let timeout = parse_timeout_argument(OsStr::new("0")).expect("parse timeout");
    assert_eq!(timeout, TransferTimeout::Disabled);
}

#[test]
fn timeout_argument_positive_sets_seconds() {
    let timeout = parse_timeout_argument(OsStr::new("15")).expect("parse timeout");
    assert_eq!(timeout.as_seconds(), NonZeroU64::new(15));
}

#[test]
fn timeout_argument_negative_reports_error() {
    let error = parse_timeout_argument(OsStr::new("-1")).unwrap_err();
    assert!(error.to_string().contains("timeout must be non-negative"));
}
