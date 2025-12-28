#![allow(clippy::needless_pass_by_value)]

use protocol::{
    CompatibilityFlags, DigestListTokens, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN,
    LegacyDaemonGreeting, LogCode, LogCodeConversionError, MessageCode, NegotiationError,
    NegotiationPrologue, NegotiationPrologueSniffer, ParseLogCodeError,
    ParseNegotiationPrologueError, ParseNegotiationPrologueErrorKind,
    ParseProtocolVersionErrorKind, ProtocolVersion, ProtocolVersionAdvertisement,
    SUPPORTED_PROTOCOL_BITMAP, SupportedProtocolNumbersIter, SupportedVersionsIter, decode_varint,
    encode_varint_to_vec, parse_legacy_daemon_greeting_bytes_details,
    parse_legacy_daemon_greeting_details, read_and_parse_legacy_daemon_greeting_details,
    read_varint, select_highest_mutual, write_varint,
};
use std::iter::FusedIterator;
use std::str::FromStr;

const NEWEST_AS_USIZE: usize = ProtocolVersion::NEWEST.as_usize();
const OLDEST_AS_USIZE: usize = ProtocolVersion::OLDEST.as_usize();

#[derive(Clone, Copy)]
struct CustomAdvertised(u8);

impl ProtocolVersionAdvertisement for CustomAdvertised {
    #[inline]
    fn into_advertised_version(self) -> u32 {
        u32::from(self.0)
    }
}

#[test]
fn custom_advertised_types_can_participate_in_negotiation() {
    let peers = [CustomAdvertised(31), CustomAdvertised(32)];
    let negotiated = select_highest_mutual(peers).expect("should negotiate successfully");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn supported_protocol_exports_remain_consistent() {
    assert_eq!(
        protocol::SUPPORTED_PROTOCOL_COUNT,
        protocol::SUPPORTED_PROTOCOLS.len(),
    );
}

#[test]
fn supported_protocols_match_upstream_order() {
    assert_eq!(protocol::SUPPORTED_PROTOCOLS, [32, 31, 30, 29, 28]);
}

#[test]
fn named_protocol_version_constants_are_exposed() {
    assert_eq!(ProtocolVersion::V32.as_u8(), 32);
    assert_eq!(ProtocolVersion::V31.as_u8(), 31);
    assert_eq!(ProtocolVersion::V30.as_u8(), 30);
    assert_eq!(ProtocolVersion::V29.as_u8(), 29);
    assert_eq!(ProtocolVersion::V28.as_u8(), 28);

    assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    assert_eq!(ProtocolVersion::V28, ProtocolVersion::OLDEST);

    let expected = [
        ProtocolVersion::V32,
        ProtocolVersion::V31,
        ProtocolVersion::V30,
        ProtocolVersion::V29,
        ProtocolVersion::V28,
    ];
    assert_eq!(expected, *ProtocolVersion::supported_versions_array());
}

#[test]
fn message_header_constants_match_upstream_definition() {
    assert_eq!(protocol::MESSAGE_HEADER_LEN, 4);
    assert_eq!(protocol::MAX_PAYLOAD_LENGTH, 0x00FF_FFFF);

    let header = protocol::MessageHeader::new(protocol::MessageCode::Info, 0)
        .expect("zero-length payloads are valid");
    assert_eq!(header.encode().len(), protocol::MESSAGE_HEADER_LEN);
}

#[test]
fn multiplex_base_constant_matches_upstream_definition() {
    assert_eq!(protocol::MPLEX_BASE, 7);
}

#[test]
fn supported_protocol_exports_cover_range() {
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers(),
        &protocol::SUPPORTED_PROTOCOLS,
    );
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers_array(),
        &protocol::SUPPORTED_PROTOCOLS,
    );
    assert_eq!(
        ProtocolVersion::supported_versions_array(),
        &ProtocolVersion::SUPPORTED_VERSIONS,
    );
    assert_eq!(
        protocol::SUPPORTED_PROTOCOLS_DISPLAY,
        ProtocolVersion::supported_protocol_numbers_display(),
    );
    assert!(ProtocolVersion::supported_protocol_numbers_iter().eq(protocol::SUPPORTED_PROTOCOLS));

    let exported_range = protocol::SUPPORTED_PROTOCOL_RANGE.clone();
    assert_eq!(ProtocolVersion::supported_range(), exported_range);

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert_eq!(oldest, *exported_range.start());
    assert_eq!(newest, *exported_range.end());
    assert_eq!(protocol::SUPPORTED_PROTOCOL_BOUNDS, (oldest, newest));
}

#[test]
fn supported_protocol_iter_types_expose_exact_size_and_double_ended_iteration() {
    fn assert_traits<I>(mut iter: I)
    where
        I: ExactSizeIterator + DoubleEndedIterator + Clone + FusedIterator,
    {
        let clone = iter.clone();
        assert_eq!(clone.len(), iter.len(), "cloned iterator must preserve len");
        let expected = (
            protocol::SUPPORTED_PROTOCOL_COUNT,
            Some(protocol::SUPPORTED_PROTOCOL_COUNT),
        );
        assert_eq!(iter.size_hint(), expected);
        assert!(iter.next().is_some(), "iterator must yield from the front");
        assert!(
            iter.next_back().is_some(),
            "iterator must yield from the back"
        );
    }

    assert_traits::<SupportedProtocolNumbersIter>(
        ProtocolVersion::supported_protocol_numbers_iter(),
    );
    assert_traits::<SupportedVersionsIter>(ProtocolVersion::supported_versions_iter());
}

#[test]
fn supported_protocol_bitmap_matches_helpers() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();
    assert_eq!(bitmap, SUPPORTED_PROTOCOL_BITMAP);

    for &version in ProtocolVersion::supported_protocol_numbers() {
        let mask = 1u64 << version;
        assert_ne!(bitmap & mask, 0, "bit for protocol {version} must be set");
    }

    let lower_mask = (1u64 << ProtocolVersion::OLDEST.as_u8()) - 1;
    assert_eq!(
        bitmap & lower_mask,
        0,
        "no bits below oldest supported version"
    );

    let upper_shift = usize::from(ProtocolVersion::NEWEST.as_u8()) + 1;
    assert_eq!(
        bitmap >> upper_shift,
        0,
        "no bits above newest supported version"
    );
}

#[test]
fn digest_list_tokens_are_public_iterators() {
    fn assert_traits<'a, I>(mut iter: I)
    where
        I: Iterator<Item = &'a str> + Clone + FusedIterator,
    {
        let mut clone = iter.clone();
        assert_eq!(clone.next(), Some("md5"));
        assert_eq!(clone.next(), Some("md4"));
        assert!(clone.next().is_none());
        assert!(iter.next().is_some());
    }

    let greeting =
        parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md5 md4\n").expect("greeting parses");
    let tokens = greeting.digest_tokens();
    let _: DigestListTokens<'_> = tokens.clone();
    assert_traits(tokens);
}

#[test]
fn compatibility_flags_public_constants_are_available() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES;
    assert!(flags.contains(CompatibilityFlags::SYMLINK_TIMES));
    assert_eq!(flags.bits(), 0b11);

    let masked = flags.difference(CompatibilityFlags::SYMLINK_TIMES);
    assert_eq!(masked, CompatibilityFlags::INC_RECURSE);
}

#[test]
fn varint_codec_round_trips_through_public_api() {
    let mut encoded = Vec::new();
    write_varint(&mut encoded, 16_384).expect("write succeeds");
    let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 16_384);
    assert!(remainder.is_empty());

    let mut cursor = std::io::Cursor::new(encoded);
    let read_back = read_varint(&mut cursor).expect("read succeeds");
    assert_eq!(read_back, 16_384);

    let mut appended = Vec::new();
    encode_varint_to_vec(-1, &mut appended);
    assert_eq!(decode_varint(&appended).expect("decode succeeds").0, -1);
}

#[test]
fn protocol_version_from_str_matches_upstream_rules() {
    let trimmed = ProtocolVersion::from_str(" 31 ").expect("whitespace should be ignored");
    assert_eq!(trimmed, ProtocolVersion::V31);

    let leading_plus = ProtocolVersion::from_str("+32").expect("leading plus must be accepted");
    assert_eq!(leading_plus, ProtocolVersion::V32);

    let unsupported_future =
        ProtocolVersion::from_str("40").expect_err("future version reports unsupported range");
    assert_eq!(
        unsupported_future.kind(),
        ParseProtocolVersionErrorKind::UnsupportedRange(40),
    );
    assert_eq!(unsupported_future.unsupported_value(), Some(40));

    let zero = ProtocolVersion::from_str("0").expect_err("protocol 0 is reserved");
    assert_eq!(
        zero.kind(),
        ParseProtocolVersionErrorKind::UnsupportedRange(0)
    );
    assert_eq!(zero.unsupported_value(), Some(0));

    let negative = ProtocolVersion::from_str("-29").expect_err("negative values are rejected");
    assert_eq!(negative.kind(), ParseProtocolVersionErrorKind::Negative);

    let invalid_digit = ProtocolVersion::from_str("3x")
        .expect_err("non-digit characters trigger an invalid digit error");
    assert_eq!(
        invalid_digit.kind(),
        ParseProtocolVersionErrorKind::InvalidDigit
    );

    let empty = ProtocolVersion::from_str("   ").expect_err("empty strings are rejected");
    assert_eq!(empty.kind(), ParseProtocolVersionErrorKind::Empty);

    let overflow =
        ProtocolVersion::from_str("999").expect_err("values above u8::MAX report overflow");
    assert_eq!(overflow.kind(), ParseProtocolVersionErrorKind::Overflow);
}

#[test]
fn supported_version_lookup_by_index_matches_slice() {
    for (index, &version) in ProtocolVersion::supported_versions().iter().enumerate() {
        assert_eq!(ProtocolVersion::from_supported_index(index), Some(version));
    }

    assert!(ProtocolVersion::from_supported_index(protocol::SUPPORTED_PROTOCOL_COUNT).is_none());
}

#[test]
fn protocol_version_offsets_track_supported_ordering() {
    for (ascending_index, version) in ProtocolVersion::supported_versions()
        .iter()
        .rev()
        .enumerate()
    {
        assert_eq!(version.offset_from_oldest(), ascending_index);
    }

    for (descending_index, version) in ProtocolVersion::supported_versions().iter().enumerate() {
        assert_eq!(version.offset_from_newest(), descending_index);
    }
}

#[test]
fn protocol_version_as_usize_exposes_const_index() {
    assert_eq!(NEWEST_AS_USIZE, ProtocolVersion::NEWEST.as_u8() as usize);
    assert_eq!(OLDEST_AS_USIZE, ProtocolVersion::OLDEST.as_u8() as usize);

    for version in ProtocolVersion::supported_versions_iter() {
        assert_eq!(version.as_usize(), version.as_u8() as usize);
    }
}

#[test]
fn legacy_daemon_prefix_constants_are_public() {
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX, "@RSYNCD:");
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX_LEN, 8);
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX_BYTES, b"@RSYNCD:");
}

#[test]
fn legacy_daemon_greeting_details_are_exposed() {
    let greeting: LegacyDaemonGreeting =
        parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
            .expect("greeting should parse");
    assert_eq!(
        greeting.protocol(),
        ProtocolVersion::from_supported(31).unwrap()
    );
    assert_eq!(greeting.digest_list(), Some("md4 md5"));
    assert!(greeting.has_subprotocol());
    assert_eq!(greeting.subprotocol_raw(), Some(0));

    let bytes: LegacyDaemonGreeting = parse_legacy_daemon_greeting_bytes_details(b"@RSYNCD: 29\n")
        .expect("byte parser should parse");
    assert_eq!(bytes.protocol().as_u8(), 29);
    assert!(!bytes.has_subprotocol());
    assert_eq!(bytes.subprotocol_raw(), None);

    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = std::io::Cursor::new(b"@RSYNCD: 31.0\n".to_vec());
    let mut line = Vec::new();
    sniffer
        .read_from(&mut reader)
        .expect("sniffing succeeds for legacy greeting");
    let parsed =
        read_and_parse_legacy_daemon_greeting_details(&mut sniffer, &mut reader, &mut line)
            .expect("read parser should succeed");
    assert_eq!(parsed.protocol().as_u8(), 31);
}

#[test]
fn log_code_round_trips_between_numeric_and_name() {
    for (index, &code) in LogCode::all().iter().enumerate() {
        let numeric = u8::from(code);
        assert_eq!(
            numeric, index as u8,
            "numeric order must match upstream table"
        );

        let parsed_from_numeric = LogCode::try_from(numeric).expect("numeric value should parse");
        assert_eq!(parsed_from_numeric, code);

        let name = code.name();
        let parsed_from_name = name.parse::<LogCode>().expect("name should parse");
        assert_eq!(parsed_from_name, code);

        // Display uses the upstream mnemonic identifier verbatim.
        assert_eq!(code.to_string(), name);
    }
}

#[test]
fn parse_log_code_error_reports_invalid_numeric_values() {
    let err = LogCode::try_from(9).expect_err("value above table must fail");

    assert!(matches!(err, ParseLogCodeError::InvalidValue(9)));
    assert_eq!(err.invalid_value(), Some(9));
    assert_eq!(err.invalid_name(), None);
    assert_eq!(err.to_string(), "unknown log code value: 9");
}

#[test]
fn parse_log_code_error_reports_invalid_names() {
    let err = "NOTREAL"
        .parse::<LogCode>()
        .expect_err("unknown name must fail");

    match &err {
        ParseLogCodeError::InvalidName(name) => {
            assert_eq!(name, "NOTREAL");
        }
        other => panic!("unexpected parse error variant: {other:?}"),
    }

    // The borrowed accessors continue to work even after pattern matching.
    assert_eq!(err.invalid_name(), Some("NOTREAL"));
    assert_eq!(err.invalid_value(), None);
    assert_eq!(err.to_string(), "unknown log code name: \"NOTREAL\"");
}

#[test]
fn log_code_conversion_error_exposes_context() {
    let err = LogCodeConversionError::NoLogEquivalent(MessageCode::Data);
    assert_eq!(err.log_code(), None);
    assert_eq!(err.message_code(), Some(MessageCode::Data));
    assert_eq!(
        err.to_string(),
        "message code MSG_DATA has no log code equivalent"
    );
}

#[test]
fn message_code_logging_variants_round_trip_with_log_codes() {
    for &code in MessageCode::all().iter() {
        match code.log_code() {
            Some(log) => {
                let from_log = MessageCode::from_log_code(log)
                    .expect("log code should map back to a message code");
                assert_eq!(from_log, code, "log round-trip must match original code");
            }
            None => {
                assert!(MessageCode::from_log_code(LogCode::None).is_none());
            }
        }
    }

    for &log in LogCode::all().iter() {
        match MessageCode::from_log_code(log) {
            Some(code) => {
                assert_eq!(code.log_code(), Some(log));
            }
            None => {
                assert_eq!(log, LogCode::None, "only FNONE lacks a multiplexed variant");
            }
        }
    }
}

#[test]
fn message_code_flush_alias_matches_info_variant() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());
    assert_eq!(MessageCode::FLUSH.name(), MessageCode::Info.name());
    assert_eq!(
        "MSG_FLUSH"
            .parse::<MessageCode>()
            .expect("alias should parse"),
        MessageCode::Info
    );
}

#[test]
fn negotiation_prologue_sniffer_reports_buffered_length() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX_BYTES);

    let mut replay = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_buffered_into_slice(&mut replay)
        .expect("slice large enough to hold legacy prefix");
    assert_eq!(copied, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&replay, LEGACY_DAEMON_PREFIX_BYTES);

    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert!(sniffer.buffered().is_empty());
}

#[test]
fn negotiation_prologue_from_str_is_part_of_public_api() {
    let parsed: NegotiationPrologue = "binary".parse().expect("identifier should parse");
    assert!(parsed.is_binary());

    let err: ParseNegotiationPrologueError = "unknown"
        .parse::<NegotiationPrologue>()
        .expect_err("unknown value should fail");
    assert_eq!(err.kind(), ParseNegotiationPrologueErrorKind::Invalid);
}

#[test]
fn negotiation_error_accessors_are_public() {
    let err = NegotiationError::NoMutualProtocol {
        peer_versions: vec![30, 31],
    };
    assert_eq!(err.peer_versions(), Some(&[30, 31][..]));
    assert_eq!(err.unsupported_version(), None);
    assert_eq!(err.malformed_legacy_greeting(), None);
}
