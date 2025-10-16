#![allow(clippy::needless_pass_by_value)]

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN, LogCode, LogCodeConversionError,
    MessageCode, NegotiationError, NegotiationPrologue, NegotiationPrologueSniffer,
    ParseLogCodeError, ProtocolVersion, ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP,
    SupportedProtocolNumbersIter, SupportedVersionsIter, select_highest_mutual,
};
use std::iter::FusedIterator;

#[derive(Clone, Copy)]
struct CustomAdvertised(u8);

impl ProtocolVersionAdvertisement for CustomAdvertised {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.0
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
        rsync_protocol::SUPPORTED_PROTOCOL_COUNT,
        rsync_protocol::SUPPORTED_PROTOCOLS.len(),
    );
}

#[test]
fn supported_protocols_match_upstream_order() {
    assert_eq!(rsync_protocol::SUPPORTED_PROTOCOLS, [32, 31, 30, 29, 28]);
}

#[test]
fn message_header_constants_match_upstream_definition() {
    assert_eq!(rsync_protocol::MESSAGE_HEADER_LEN, 4);
    assert_eq!(rsync_protocol::MAX_PAYLOAD_LENGTH, 0x00FF_FFFF);

    let header = rsync_protocol::MessageHeader::new(rsync_protocol::MessageCode::Info, 0)
        .expect("zero-length payloads are valid");
    assert_eq!(header.encode().len(), rsync_protocol::MESSAGE_HEADER_LEN);
}

#[test]
fn supported_protocol_exports_cover_range() {
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers(),
        &rsync_protocol::SUPPORTED_PROTOCOLS,
    );
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers_array(),
        &rsync_protocol::SUPPORTED_PROTOCOLS,
    );
    assert_eq!(
        ProtocolVersion::supported_versions_array(),
        &ProtocolVersion::SUPPORTED_VERSIONS,
    );
    assert!(
        ProtocolVersion::supported_protocol_numbers_iter().eq(rsync_protocol::SUPPORTED_PROTOCOLS)
    );

    let exported_range = rsync_protocol::SUPPORTED_PROTOCOL_RANGE.clone();
    assert_eq!(ProtocolVersion::supported_range(), exported_range.clone());

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert_eq!(oldest, *exported_range.start());
    assert_eq!(newest, *exported_range.end());
    assert_eq!(rsync_protocol::SUPPORTED_PROTOCOL_BOUNDS, (oldest, newest));
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
            rsync_protocol::SUPPORTED_PROTOCOL_COUNT,
            Some(rsync_protocol::SUPPORTED_PROTOCOL_COUNT),
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
fn legacy_daemon_prefix_constants_are_public() {
    assert_eq!(rsync_protocol::LEGACY_DAEMON_PREFIX, "@RSYNCD:");
    assert_eq!(rsync_protocol::LEGACY_DAEMON_PREFIX_LEN, 8);
    assert_eq!(rsync_protocol::LEGACY_DAEMON_PREFIX_BYTES, b"@RSYNCD:");
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
fn negotiation_error_accessors_are_public() {
    let err = NegotiationError::NoMutualProtocol {
        peer_versions: vec![30, 31],
    };
    assert_eq!(err.peer_versions(), Some(&[30, 31][..]));
    assert_eq!(err.unsupported_version(), None);
    assert_eq!(err.malformed_legacy_greeting(), None);
}
