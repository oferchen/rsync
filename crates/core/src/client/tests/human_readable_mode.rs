use crate::client::{HumanReadableMode, HumanReadableModeParseError};
use std::str::FromStr;

#[test]
fn human_readable_mode_parses_numeric_levels() {
    assert_eq!(HumanReadableMode::parse("0").unwrap(), HumanReadableMode::Disabled);
    assert_eq!(HumanReadableMode::parse("1").unwrap(), HumanReadableMode::Enabled);
    assert_eq!(HumanReadableMode::parse("2").unwrap(), HumanReadableMode::Combined);
}

#[test]
fn human_readable_mode_trims_ascii_whitespace() {
    assert_eq!(
        HumanReadableMode::parse(" 1 \t").unwrap(),
        HumanReadableMode::Enabled
    );
    assert_eq!(
        HumanReadableMode::from_str("\n 2  \r").unwrap(),
        HumanReadableMode::Combined
    );
}

#[test]
fn human_readable_mode_rejects_empty_values() {
    let error = HumanReadableMode::parse("   ").unwrap_err();
    assert_eq!(error, HumanReadableModeParseError::Empty);
    assert_eq!(error.to_string(), "human-readable level must not be empty");
    assert_eq!(error.invalid_value(), None);
}

#[test]
fn human_readable_mode_reports_invalid_values() {
    let error = HumanReadableMode::parse(" 9 ").unwrap_err();
    assert_eq!(
        error,
        HumanReadableModeParseError::Invalid {
            value: String::from("9"),
        }
    );
    assert_eq!(
        error.to_string(),
        "invalid human-readable level '9': expected 0, 1, or 2"
    );
    assert_eq!(error.invalid_value(), Some("9"));
}
