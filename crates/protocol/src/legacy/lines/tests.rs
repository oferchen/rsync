use super::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn assert_copy<T: Copy>() {}
fn assert_hash<T: Hash>() {}

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn legacy_daemon_message_supports_copy_and_hash() {
    assert_copy::<LegacyDaemonMessage<'static>>();
    assert_hash::<LegacyDaemonMessage<'static>>();

    let sample = LegacyDaemonMessage::AuthRequired {
        module: Some("module"),
    };
    let copied = sample;

    assert_eq!(sample, copied);
    assert_eq!(hash_value(&sample), hash_value(&copied));
}

#[test]
fn parse_legacy_daemon_message_accepts_ok_keyword() {
    let message = parse_legacy_daemon_message("@RSYNCD: OK\r\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Ok);
}

#[test]
fn parse_legacy_daemon_message_accepts_ok_with_trailing_whitespace() {
    let message = parse_legacy_daemon_message("@RSYNCD: OK   \r\n").expect("keyword with padding");
    assert_eq!(message, LegacyDaemonMessage::Ok);
}

#[test]
fn parse_legacy_daemon_message_accepts_exit_keyword() {
    let message = parse_legacy_daemon_message("@RSYNCD: EXIT\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Exit);
}

#[test]
fn parse_legacy_daemon_message_accepts_auth_challenge_keyword() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTH abc123\n").expect("keyword");
    assert_eq!(
        message,
        LegacyDaemonMessage::AuthChallenge {
            challenge: "abc123",
        }
    );
}

#[test]
fn parse_legacy_daemon_message_rejects_auth_without_payload() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTH\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Other("AUTH"));
}

#[test]
fn parse_legacy_daemon_message_rejects_lowercase_prefix() {
    let err = parse_legacy_daemon_message("@rsyncd: OK\n").unwrap_err();
    match err {
        NegotiationError::MalformedLegacyGreeting { input } => {
            assert_eq!(input, "@rsyncd: OK");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn write_legacy_daemon_message_formats_version_branch() {
    let message = LegacyDaemonMessage::Version(ProtocolVersion::from_supported(31).unwrap());
    let rendered = format_legacy_daemon_message(message);
    assert_eq!(rendered, "@RSYNCD: 31.0\n");
}

#[test]
fn write_legacy_daemon_message_formats_keywords() {
    assert_eq!(
        format_legacy_daemon_message(LegacyDaemonMessage::Ok),
        "@RSYNCD: OK\n"
    );
    assert_eq!(
        format_legacy_daemon_message(LegacyDaemonMessage::Exit),
        "@RSYNCD: EXIT\n"
    );
    assert_eq!(
        format_legacy_daemon_message(LegacyDaemonMessage::AuthChallenge {
            challenge: "abc123",
        }),
        "@RSYNCD: AUTH abc123\n"
    );
}

#[test]
fn write_legacy_daemon_message_formats_capabilities() {
    let message = LegacyDaemonMessage::Capabilities { flags: "0x1f 0x2" };
    let rendered = format_legacy_daemon_message(message);
    assert_eq!(rendered, "@RSYNCD: CAP 0x1f 0x2\n");
}

#[test]
fn write_legacy_daemon_message_formats_auth_requests() {
    let without_module = LegacyDaemonMessage::AuthRequired { module: None };
    assert_eq!(
        format_legacy_daemon_message(without_module),
        "@RSYNCD: AUTHREQD\n"
    );

    let with_module = LegacyDaemonMessage::AuthRequired {
        module: Some("module"),
    };
    assert_eq!(
        format_legacy_daemon_message(with_module),
        "@RSYNCD: AUTHREQD module\n"
    );
}

#[test]
fn write_legacy_daemon_message_normalises_other_payloads() {
    let parsed =
        parse_legacy_daemon_message("@RSYNCD: EXTRA   \t \r\n").expect("message should parse");
    assert_eq!(format_legacy_daemon_message(parsed), "@RSYNCD: EXTRA\n");
}

#[test]
fn parse_legacy_daemon_message_accepts_exit_with_trailing_whitespace() {
    let message = parse_legacy_daemon_message("@RSYNCD: EXIT   \n").expect("keyword with padding");
    assert_eq!(message, LegacyDaemonMessage::Exit);
}

#[test]
fn parse_legacy_daemon_message_accepts_authreqd_with_module() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD sample\n").expect("keyword");
    assert_eq!(
        message,
        LegacyDaemonMessage::AuthRequired {
            module: Some("sample"),
        }
    );
}

#[test]
fn parse_legacy_daemon_message_preserves_internal_whitespace_in_module_name() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD  module name\t\r\n")
        .expect("keyword with extra whitespace");
    assert_eq!(
        message,
        LegacyDaemonMessage::AuthRequired {
            module: Some("module name"),
        }
    );
}

#[test]
fn parse_legacy_daemon_message_accepts_authreqd_without_module() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::AuthRequired { module: None });
}

#[test]
fn parse_legacy_daemon_message_requires_delimiter_after_authreqd_keyword() {
    let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQDmodule\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Other("AUTHREQDmodule"));
}

#[test]
fn parse_legacy_daemon_message_treats_whitespace_only_module_as_none() {
    let message =
        parse_legacy_daemon_message("@RSYNCD: AUTHREQD    \n").expect("keyword with padding");
    assert_eq!(message, LegacyDaemonMessage::AuthRequired { module: None });
}

#[test]
fn parse_legacy_daemon_message_accepts_authreqd_with_trailing_whitespace() {
    let message =
        parse_legacy_daemon_message("@RSYNCD: AUTHREQD module   \n").expect("keyword with padding");
    assert_eq!(
        message,
        LegacyDaemonMessage::AuthRequired {
            module: Some("module"),
        }
    );
}

#[test]
fn parse_legacy_daemon_message_accepts_capabilities_keyword() {
    let message = parse_legacy_daemon_message("@RSYNCD: CAP 0x1f 0x2\n").expect("keyword");
    assert_eq!(
        message,
        LegacyDaemonMessage::Capabilities { flags: "0x1f 0x2" }
    );
}

#[test]
fn parse_legacy_daemon_message_accepts_capabilities_with_extra_whitespace() {
    let message = parse_legacy_daemon_message("@RSYNCD: CAP\t capabilities list  \r\n")
        .expect("keyword with padding");
    assert_eq!(
        message,
        LegacyDaemonMessage::Capabilities {
            flags: "capabilities list",
        }
    );
}

#[test]
fn parse_legacy_daemon_message_rejects_capabilities_without_payload() {
    let message = parse_legacy_daemon_message("@RSYNCD: CAP\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Other("CAP"));
}

#[test]
fn parse_legacy_daemon_message_rejects_capabilities_without_delimiter() {
    let message = parse_legacy_daemon_message("@RSYNCD: CAPpayload\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Other("CAPpayload"));
}

#[test]
fn parse_legacy_daemon_message_classifies_unknown_keywords() {
    let message = parse_legacy_daemon_message("@RSYNCD: SOMETHING\n").expect("keyword");
    assert_eq!(message, LegacyDaemonMessage::Other("SOMETHING"));
}

#[test]
fn parse_legacy_daemon_message_routes_version_to_existing_parser() {
    let message = parse_legacy_daemon_message("@RSYNCD: 30.0\n").expect("version");
    assert_eq!(
        message,
        LegacyDaemonMessage::Version(ProtocolVersion::new_const(30))
    );
}

#[test]
fn parse_legacy_daemon_message_tolerates_leading_whitespace_before_version_digits() {
    let message =
        parse_legacy_daemon_message("@RSYNCD:    29.0  \r\n").expect("version with padding");
    assert_eq!(
        message,
        LegacyDaemonMessage::Version(ProtocolVersion::new_const(29))
    );
}

#[test]
fn parse_legacy_daemon_message_rejects_missing_prefix() {
    let err = parse_legacy_daemon_message("RSYNCD: AUTHREQD module\n").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn parse_legacy_daemon_message_rejects_empty_payload() {
    let err = parse_legacy_daemon_message("@RSYNCD:\n").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn parse_legacy_daemon_message_accepts_authreqd_with_trailing_tabs() {
    let message =
        parse_legacy_daemon_message("@RSYNCD: AUTHREQD\tmodule\t\n").expect("keyword with tabs");
    assert_eq!(
        message,
        LegacyDaemonMessage::AuthRequired {
            module: Some("module"),
        }
    );
}

#[test]
fn parses_legacy_error_message_and_trims_payload() {
    let payload = parse_legacy_error_message("@ERROR: access denied\r\n").expect("error payload");
    assert_eq!(payload, "access denied");
}

#[test]
fn parse_legacy_error_message_allows_empty_payload() {
    let payload = parse_legacy_error_message("@ERROR:\n").expect("empty payload");
    assert_eq!(payload, "");
}

#[test]
fn parse_legacy_error_message_returns_none_for_unrecognized_prefix() {
    assert!(parse_legacy_error_message("something else\n").is_none());
}

#[test]
fn parses_legacy_warning_message_and_trims_payload() {
    let payload =
        parse_legacy_warning_message("@WARNING: will retry  \n").expect("warning payload");
    assert_eq!(payload, "will retry");
}

#[test]
fn parse_legacy_warning_message_allows_empty_payload() {
    let payload = parse_legacy_warning_message("@WARNING:\r\n").expect("empty payload");
    assert_eq!(payload, "");
}

#[test]
fn parse_legacy_warning_message_returns_none_for_unrecognized_prefix() {
    assert!(parse_legacy_warning_message("something else\n").is_none());
}
