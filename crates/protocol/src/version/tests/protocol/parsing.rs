use super::super::{ParseProtocolVersionError, ParseProtocolVersionErrorKind, ProtocolVersion};
use core::str::FromStr;

#[test]
fn protocol_version_from_str_accepts_supported_values() {
    assert_eq!(
        ProtocolVersion::from_str("32").expect("32 is supported"),
        ProtocolVersion::NEWEST
    );
    assert_eq!(
        ProtocolVersion::from_str(" 29 ")
            .expect("whitespace should be ignored")
            .as_u8(),
        29
    );
    assert_eq!(
        ProtocolVersion::from_str("+30")
            .expect("leading plus is accepted")
            .as_u8(),
        30
    );
}

#[test]
fn protocol_version_from_str_reports_error_kinds() {
    let empty = ProtocolVersion::from_str("").unwrap_err();
    assert_eq!(empty.kind(), ParseProtocolVersionErrorKind::Empty);

    let invalid = ProtocolVersion::from_str("abc").unwrap_err();
    assert_eq!(invalid.kind(), ParseProtocolVersionErrorKind::InvalidDigit);

    let double_sign = ProtocolVersion::from_str("+-31").unwrap_err();
    assert_eq!(
        double_sign.kind(),
        ParseProtocolVersionErrorKind::InvalidDigit
    );

    let negative = ProtocolVersion::from_str("-31").unwrap_err();
    assert_eq!(negative.kind(), ParseProtocolVersionErrorKind::Negative);

    let overflow = ProtocolVersion::from_str("256").unwrap_err();
    assert_eq!(overflow.kind(), ParseProtocolVersionErrorKind::Overflow);

    let unsupported = ProtocolVersion::from_str("27").unwrap_err();
    assert_eq!(
        unsupported.kind(),
        ParseProtocolVersionErrorKind::UnsupportedRange(27)
    );
    assert_eq!(unsupported.unsupported_value(), Some(27));
}

#[test]
fn parse_protocol_version_error_display_matches_variants() {
    let empty = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
    assert_eq!(empty.to_string(), "protocol version string is empty");

    let invalid = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::InvalidDigit);
    assert_eq!(
        invalid.to_string(),
        "protocol version must be an unsigned integer"
    );

    let negative = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
    assert_eq!(negative.to_string(), "protocol version cannot be negative");

    let overflow = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow);
    assert_eq!(
        overflow.to_string(),
        "protocol version value exceeds u8::MAX"
    );

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    let unsupported =
        ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(27));
    assert_eq!(
        unsupported.to_string(),
        format!("protocol version 27 is outside the supported range {oldest}-{newest}")
    );
}
