//! Tests for legacy daemon greeting parsing and formatting.

use super::super::LEGACY_DAEMON_PREFIX;
use super::*;
use crate::{NegotiationError, ProtocolVersion};
use ::core::fmt;

#[test]
fn parses_legacy_daemon_greeting_with_minor_version() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 31.0\r\n").expect("valid greeting");
    assert_eq!(parsed.as_u8(), 31);
}

#[test]
fn legacy_daemon_greeting_exposes_optional_subprotocol() {
    let with_fractional = parse_legacy_daemon_greeting_details("@RSYNCD: 30.5\n")
        .expect("fractional component must parse");
    assert_eq!(with_fractional.subprotocol_raw(), Some(5));
    assert!(with_fractional.has_subprotocol());
    assert_eq!(with_fractional.subprotocol(), 5);

    let without_fractional =
        parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("suffix-less greeting");
    assert_eq!(without_fractional.subprotocol_raw(), None);
    assert!(!without_fractional.has_subprotocol());
    assert_eq!(without_fractional.subprotocol(), 0);
}

#[test]
fn parses_legacy_daemon_greeting_without_space_after_prefix() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD:31.0\n").expect("valid greeting");
    assert_eq!(parsed.as_u8(), 31);
}

#[test]
fn parses_legacy_daemon_greeting_with_whitespace_before_fractional() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 32   .0   \n").expect("valid greeting");
    assert_eq!(parsed, ProtocolVersion::NEWEST);
}

#[test]
fn parses_legacy_daemon_greeting_without_fractional_suffix() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30\n").expect("fractional optional");
    assert_eq!(parsed.as_u8(), 30);
}

#[test]
fn parses_legacy_daemon_greeting_details_with_digest_list() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
        .expect("digest list should parse");

    assert_eq!(
        greeting.protocol(),
        ProtocolVersion::from_supported(31).unwrap()
    );
    assert_eq!(greeting.advertised_protocol(), 31);
    assert!(greeting.has_subprotocol());
    assert_eq!(greeting.subprotocol(), 0);
    assert_eq!(greeting.digest_list(), Some("md4 md5"));
    assert!(greeting.has_digest_list());
}

#[test]
fn greeting_details_accepts_trailing_whitespace_in_digest_list() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0   md4   md5  \r\n")
        .expect("digest list should tolerate padding");

    assert_eq!(greeting.digest_list(), Some("md4   md5"));
    assert!(greeting.has_digest_list());
}

#[test]
fn borrowed_greeting_digest_tokens_iterate_in_order() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 30.0 md5\tmd4\n")
        .expect("digest list should parse");

    let collected: Vec<_> = greeting.digest_tokens().collect();
    assert_eq!(collected, ["md5", "md4"]);

    let no_digest = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("no digest list");
    let empty: Vec<_> = no_digest.digest_tokens().collect();
    assert!(empty.is_empty());
}

#[test]
fn borrowed_greeting_reports_supported_digests() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md5 md4\n")
        .expect("digest list should parse");

    assert!(greeting.supports_digest("md5"));
    assert!(greeting.supports_digest(" MD4 "));
    assert!(!greeting.supports_digest("sha1"));
    assert!(!greeting.supports_digest(""));

    let no_digest = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("no digest list");
    assert!(!no_digest.supports_digest("md5"));
}

#[test]
fn greeting_details_records_absence_of_subprotocol_for_old_versions() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n")
        .expect("old protocols may omit subprotocol");

    assert_eq!(greeting.protocol().as_u8(), 29);
    assert!(!greeting.has_subprotocol());
    assert_eq!(greeting.subprotocol(), 0);
    assert!(!greeting.has_digest_list());
}

#[test]
fn parse_owned_greeting_retains_metadata() {
    let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29.1 md4\n")
        .expect("owned parsing should succeed");

    assert_eq!(
        owned.protocol(),
        ProtocolVersion::from_supported(29).unwrap()
    );
    assert_eq!(owned.advertised_protocol(), 29);
    assert_eq!(owned.subprotocol_raw(), Some(1));
    assert_eq!(owned.digest_list(), Some("md4"));
    assert!(owned.has_digest_list());
}

#[test]
fn owned_greeting_digest_tokens_iterate_in_order() {
    let greeting =
        LegacyDaemonGreetingOwned::from_parts(31, Some(0), Some(" md4  md5  md6".into()))
            .expect("construction succeeds");

    let collected: Vec<_> = greeting.digest_tokens().collect();
    assert_eq!(collected, ["md4", "md5", "md6"]);

    let no_digest = LegacyDaemonGreetingOwned::from_parts(28, None, None).expect("no digest list");
    let empty: Vec<_> = no_digest.digest_tokens().collect();
    assert!(empty.is_empty());
}

#[test]
fn owned_greeting_reports_supported_digests() {
    let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
        .expect("digest list should parse")
        .into_owned();

    assert!(greeting.supports_digest("md4"));
    assert!(greeting.supports_digest("MD5"));
    assert!(!greeting.supports_digest("sha1"));

    let no_digest = parse_legacy_daemon_greeting_details("@RSYNCD: 30.0\n")
        .expect("no digest list")
        .into_owned();
    assert!(!no_digest.supports_digest("md5"));
}

#[test]
fn owned_greeting_captures_digest_list_and_subprotocol() {
    let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 31.5 md4 md5\n")
        .expect("greeting should parse");
    let owned = LegacyDaemonGreetingOwned::from(borrowed);

    assert_eq!(owned.protocol(), borrowed.protocol());
    assert_eq!(owned.advertised_protocol(), borrowed.advertised_protocol());
    assert_eq!(owned.subprotocol_raw(), borrowed.subprotocol_raw());
    assert_eq!(owned.digest_list(), borrowed.digest_list());
    assert!(owned.has_subprotocol());
    assert!(owned.has_digest_list());

    let reborrowed = owned.as_borrowed();
    assert_eq!(reborrowed.protocol(), borrowed.protocol());
    assert_eq!(reborrowed.digest_list(), borrowed.digest_list());
}

#[test]
fn owned_greeting_tracks_absent_fields() {
    let borrowed = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n").expect("greeting");
    let owned = LegacyDaemonGreetingOwned::from(borrowed);

    assert_eq!(owned.protocol().as_u8(), 29);
    assert!(!owned.has_subprotocol());
    assert_eq!(owned.subprotocol_raw(), None);
    assert!(owned.digest_list().is_none());
    assert!(!owned.has_digest_list());
}

#[test]
fn borrowed_into_owned_preserves_metadata_without_cloning() {
    let borrowed =
        parse_legacy_daemon_greeting_details("@RSYNCD: 30.7 md5\n").expect("greeting should parse");
    let owned = borrowed.into_owned();

    assert_eq!(owned.protocol(), borrowed.protocol());
    assert_eq!(owned.advertised_protocol(), borrowed.advertised_protocol());
    assert_eq!(owned.subprotocol_raw(), borrowed.subprotocol_raw());
    assert_eq!(owned.digest_list(), borrowed.digest_list());
}

#[test]
fn into_parts_moves_digest_list_without_extra_allocations() {
    let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 28.1 md4 md5\n")
        .expect("owned parsing should succeed");
    let (protocol, advertised, subprotocol, digest) = owned.into_parts();

    assert_eq!(protocol, ProtocolVersion::from_supported(28).unwrap());
    assert_eq!(advertised, 28);
    assert_eq!(subprotocol, Some(1));
    assert_eq!(digest, Some(String::from("md4 md5")));
}

#[test]
fn into_digest_list_consumes_greeting() {
    let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29.9 md4\n")
        .expect("owned parsing should succeed");
    let digest = owned.into_digest_list();

    assert_eq!(digest, Some(String::from("md4")));
}

#[test]
fn from_parts_replicates_parser_behaviour() {
    let constructed =
        LegacyDaemonGreetingOwned::from_parts(31, Some(0), Some(String::from("  md4   md5  ")))
            .expect("construction should succeed");
    let parsed = parse_legacy_daemon_greeting_owned("@RSYNCD: 31.0   md4   md5  \r\n")
        .expect("parsing should succeed");

    assert_eq!(constructed.protocol(), parsed.protocol());
    assert_eq!(
        constructed.advertised_protocol(),
        parsed.advertised_protocol()
    );
    assert_eq!(constructed.subprotocol_raw(), parsed.subprotocol_raw());
    assert_eq!(constructed.digest_list(), parsed.digest_list());
}

#[test]
fn from_parts_requires_subprotocol_for_newer_versions() {
    let err = LegacyDaemonGreetingOwned::from_parts(31, None, Some(String::from("md4")))
        .expect_err("missing subprotocol must error");
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { input } if input == "@RSYNCD: 31 md4"
    ));
}

#[test]
fn from_parts_accepts_cap_future_versions() {
    let constructed = LegacyDaemonGreetingOwned::from_parts(40, Some(1), None)
        .expect("future advertisement at upstream cap should clamp");

    assert_eq!(constructed.protocol(), ProtocolVersion::NEWEST);
    assert_eq!(constructed.advertised_protocol(), 40);
    assert_eq!(constructed.subprotocol_raw(), Some(1));
}

#[test]
fn from_parts_rejects_advertisements_beyond_cap() {
    let err = LegacyDaemonGreetingOwned::from_parts(999, Some(1), None).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(999));
}

#[test]
fn greeting_details_rejects_missing_subprotocol_for_newer_versions() {
    let err = parse_legacy_daemon_greeting_details("@RSYNCD: 31\n").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn greeting_details_clamps_future_versions_but_retains_advertisement() {
    let greeting =
        parse_legacy_daemon_greeting_details("@RSYNCD: 40.1\n").expect("cap future versions clamp");

    assert_eq!(greeting.protocol(), ProtocolVersion::NEWEST);
    assert_eq!(greeting.advertised_protocol(), 40);
    assert_eq!(greeting.subprotocol(), 1);
}

#[test]
fn greeting_details_rejects_versions_beyond_cap() {
    let err = parse_legacy_daemon_greeting_details("@RSYNCD: 999.1\n").unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(999));
}

#[test]
fn parses_greeting_with_trailing_whitespace() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30.0   \n").expect("valid greeting");
    assert_eq!(parsed.as_u8(), 30);
}

#[test]
fn rejects_greeting_with_unsupported_version() {
    let err = parse_legacy_daemon_greeting("@RSYNCD: 27.0").unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(27));
}

#[test]
fn clamps_future_versions_in_legacy_greeting() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 40.1\n").expect("must clamp");
    assert_eq!(parsed, ProtocolVersion::NEWEST);
}

#[test]
fn parses_large_future_version_numbers_by_clamping() {
    let parsed = parse_legacy_daemon_greeting("@RSYNCD: 40.1\n").expect("must clamp at cap");
    assert_eq!(parsed, ProtocolVersion::NEWEST);
}

#[test]
fn rejects_future_versions_beyond_cap() {
    let err = parse_legacy_daemon_greeting("@RSYNCD: 4294967295.0\n").unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(u32::MAX));
}

#[test]
fn rejects_greeting_with_missing_prefix() {
    let err = parse_legacy_daemon_greeting("RSYNCD 32").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn rejects_greeting_without_version_digits() {
    let err = parse_legacy_daemon_greeting("@RSYNCD: .0").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn rejects_greeting_with_fractional_without_digits() {
    let err = parse_legacy_daemon_greeting("@RSYNCD: 31.\n").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn rejects_greeting_with_non_numeric_suffix() {
    let err = parse_legacy_daemon_greeting("@RSYNCD: 31.0beta").unwrap_err();
    assert!(matches!(
        err,
        NegotiationError::MalformedLegacyGreeting { .. }
    ));
}

#[test]
fn rejects_greeting_with_lowercase_prefix() {
    let err = parse_legacy_daemon_greeting("@rsyncd: 31.0\n").unwrap_err();
    match err {
        NegotiationError::MalformedLegacyGreeting { input } => {
            assert_eq!(input, "@rsyncd: 31.0");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn formats_legacy_daemon_greeting_for_newest_protocol() {
    let rendered = format_legacy_daemon_greeting(ProtocolVersion::NEWEST);
    assert_eq!(rendered, "@RSYNCD: 32.0\n");
}

#[test]
fn formatted_legacy_greeting_round_trips_through_parser() {
    for &version in ProtocolVersion::supported_versions() {
        let rendered = format_legacy_daemon_greeting(version);
        let parsed = parse_legacy_daemon_greeting(&rendered)
            .unwrap_or_else(|err| panic!("failed to parse {rendered:?}: {err}"));
        assert_eq!(parsed, version);
    }
}

#[test]
fn write_legacy_daemon_greeting_matches_formatter() {
    for &version in ProtocolVersion::supported_versions() {
        let mut rendered = String::new();
        write_legacy_daemon_greeting(&mut rendered, version).expect("writing to String");
        assert_eq!(rendered, format_legacy_daemon_greeting(version));
    }
}

#[test]
fn write_legacy_daemon_greeting_propagates_errors() {
    struct FailingWriter {
        remaining: usize,
    }

    impl fmt::Write for FailingWriter {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            if self.remaining < s.len() {
                self.remaining = 0;
                return Err(fmt::Error);
            }
            self.remaining -= s.len();
            Ok(())
        }

        fn write_char(&mut self, ch: char) -> fmt::Result {
            let needed = ch.len_utf8();
            if self.remaining < needed {
                self.remaining = 0;
                return Err(fmt::Error);
            }
            self.remaining -= needed;
            Ok(())
        }
    }

    let mut writer = FailingWriter {
        remaining: LEGACY_DAEMON_PREFIX.len(),
    };
    assert!(write_legacy_daemon_greeting(&mut writer, ProtocolVersion::NEWEST).is_err());
}
