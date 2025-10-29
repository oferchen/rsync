use super::*;
use proptest::prelude::*;

#[test]
fn header_round_trips_for_info_message() {
    let header = MessageHeader::new(MessageCode::Info, 123).expect("constructible header");
    let encoded = header.encode();
    let decoded = MessageHeader::decode(&encoded).expect("decode succeeds");
    assert_eq!(decoded, header);
}

#[test]
fn message_header_try_from_array_round_trips() {
    let header = MessageHeader::new(MessageCode::Data, 17).expect("constructible header");
    let encoded = header.encode();
    let decoded = MessageHeader::try_from(encoded).expect("array conversion succeeds");

    assert_eq!(decoded, header);
}

#[test]
fn message_header_try_from_slice_round_trips() {
    let header = MessageHeader::new(MessageCode::Info, 7).expect("constructible header");
    let encoded = header.encode();
    let decoded = MessageHeader::try_from(&encoded).expect("slice conversion succeeds");

    assert_eq!(decoded, header);
}

#[test]
fn message_header_try_from_array_rejects_invalid_tag() {
    let encoded = [0u8; HEADER_LEN];
    let err = MessageHeader::try_from(encoded).expect_err("invalid tag must fail");

    assert_eq!(err, EnvelopeError::InvalidTag(0));
}

#[test]
fn message_header_new_supports_const_contexts() {
    const HEADER: MessageHeader = match MessageHeader::new(MessageCode::Info, 42) {
        Ok(header) => header,
        Err(_) => panic!("valid header should be constructible in const context"),
    };

    assert_eq!(HEADER.code(), MessageCode::Info);
    assert_eq!(HEADER.payload_len(), 42);
}

#[test]
fn message_header_from_raw_round_trips() {
    let header = MessageHeader::new(MessageCode::Stats, 0x0055_AA11).expect("constructible header");
    let raw = header.encode_raw();
    let decoded = MessageHeader::from_raw(raw).expect("raw representation decodes");

    assert_eq!(decoded, header);
}

#[test]
fn message_header_from_raw_rejects_invalid_tag() {
    let raw = 0x0000_0001u32; // tag without MPLEX_BASE offset
    let err = MessageHeader::from_raw(raw).expect_err("invalid tag must fail");

    assert_eq!(err, EnvelopeError::InvalidTag(0));
}

#[test]
fn message_header_from_raw_rejects_unknown_code() {
    let raw = ((u32::from(MPLEX_BASE) + 0x40) << 24) | 0x0000_00FF;
    let err = MessageHeader::from_raw(raw).expect_err("unknown code must fail");

    assert_eq!(err, EnvelopeError::UnknownMessageCode(0x40));
}

#[test]
fn message_header_encode_raw_matches_encode() {
    let header = MessageHeader::new(MessageCode::Success, 7).expect("constructible header");
    let encoded = header.encode();
    let raw = header.encode_raw();

    assert_eq!(encoded, raw.to_le_bytes());
}

#[test]
fn try_from_log_code_converts_logging_variants() {
    for &log in LogCode::all() {
        match log {
            LogCode::None => {
                let err = MessageCode::try_from(log).expect_err("FNONE has no multiplexed tag");
                assert_eq!(
                    err,
                    LogCodeConversionError::NoMessageEquivalent(LogCode::None)
                );
            }
            _ => {
                let message = MessageCode::try_from(log).expect("log code has multiplexed tag");
                assert_eq!(message.log_code(), Some(log));
            }
        }
    }
}

#[test]
fn try_from_message_code_rejects_non_logging_variants() {
    for &code in MessageCode::all() {
        match code.log_code() {
            Some(log) => {
                let parsed = LogCode::try_from(code).expect("logging code maps to log code");
                assert_eq!(parsed, log);
            }
            None => {
                let err = LogCode::try_from(code).expect_err("non-logging message lacks log code");
                assert_eq!(err, LogCodeConversionError::NoLogEquivalent(code));
            }
        }
    }
}

#[test]
fn encode_into_slice_writes_bytes_without_touching_tail() {
    let header =
        MessageHeader::new(MessageCode::Info, 0x0055_AA11).expect("payload fits within header");
    let mut buffer = [0xFFu8; HEADER_LEN + 3];

    header
        .encode_into_slice(&mut buffer)
        .expect("buffer large enough for header");

    assert_eq!(&buffer[..HEADER_LEN], &header.encode());
    assert_eq!(buffer[HEADER_LEN..], [0xFFu8; 3]);
}

#[test]
fn encode_into_slice_rejects_short_buffers() {
    let header = MessageHeader::new(MessageCode::Info, 7).expect("valid header");
    let mut buffer = [0u8; HEADER_LEN - 1];

    let err = header
        .encode_into_slice(&mut buffer)
        .expect_err("insufficient buffer should error");

    assert_eq!(
        err,
        EnvelopeError::TruncatedHeader {
            actual: HEADER_LEN - 1
        }
    );
}

#[test]
fn payload_len_usize_matches_u32_accessor() {
    if usize::BITS < 24 {
        // Architectures with pointer widths below 24 bits cannot represent the full
        // multiplexed payload range. The implementation itself guards this via a debug
        // assertion, so skip the comparison in that niche configuration.
        return;
    }

    let header = MessageHeader::new(MessageCode::Data, MAX_PAYLOAD_LENGTH).expect("max payload");
    assert_eq!(header.payload_len_usize(), header.payload_len() as usize);
}

#[test]
fn message_header_encode_supports_const_contexts() {
    const HEADER: MessageHeader = match MessageHeader::new(MessageCode::Warning, 7) {
        Ok(header) => header,
        Err(_) => panic!("valid header should be constructible in const context"),
    };
    const ENCODED: [u8; HEADER_LEN] = HEADER.encode();

    assert_eq!(ENCODED, HEADER.encode());
}

#[test]
fn log_codes_are_hashable() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    assert!(set.insert(LogCode::Info));
    assert!(set.contains(&LogCode::Info));
    assert!(!set.insert(LogCode::Info));
}

#[test]
fn message_codes_are_hashable() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    assert!(set.insert(MessageCode::Data));
    assert!(set.contains(&MessageCode::Data));
    assert!(!set.insert(MessageCode::Data));
}

#[test]
fn decode_rejects_truncated_header() {
    let err = MessageHeader::decode(&[0u8; 2]).unwrap_err();
    assert_eq!(err, EnvelopeError::TruncatedHeader { actual: 2 });
}

#[test]
fn decode_rejects_tag_without_base_offset() {
    let raw = (u32::from(MPLEX_BASE - 1) << 24) | 1;
    let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
    assert_eq!(err, EnvelopeError::InvalidTag(MPLEX_BASE - 1));
}

#[test]
fn decode_rejects_unknown_message_codes() {
    let unknown_code = 11u8;
    let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code);
    let raw = (tag << 24) | 5;
    let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
    assert_eq!(err, EnvelopeError::UnknownMessageCode(unknown_code));
}

#[test]
fn encode_uses_little_endian_layout() {
    let payload_len = 0x00A1_B2C3;
    let header = MessageHeader::new(MessageCode::Info, payload_len).expect("constructible header");
    let encoded = header.encode();

    let expected_raw =
        ((u32::from(MPLEX_BASE) + u32::from(MessageCode::Info.as_u8())) << 24) | payload_len;
    assert_eq!(encoded, expected_raw.to_le_bytes());
}

#[test]
fn decode_masks_payload_length_to_24_bits() {
    let tag = (u32::from(MPLEX_BASE) + u32::from(MessageCode::Info.as_u8())) << 24;
    let raw = tag | (MAX_PAYLOAD_LENGTH + 1);
    let header =
        MessageHeader::decode(&raw.to_le_bytes()).expect("payload length is masked to 24 bits");
    assert_eq!(header.code(), MessageCode::Info);
    assert_eq!(
        header.payload_len(),
        (MAX_PAYLOAD_LENGTH + 1) & PAYLOAD_MASK
    );
}

#[test]
fn new_rejects_oversized_payloads() {
    let err = MessageHeader::new(MessageCode::Info, MAX_PAYLOAD_LENGTH + 1).unwrap_err();
    assert_eq!(err, EnvelopeError::OversizedPayload(MAX_PAYLOAD_LENGTH + 1));
}

#[test]
fn message_code_variants_round_trip_through_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        let decoded = MessageCode::try_from(raw).expect("known code");
        assert_eq!(decoded, code);
    }
}

#[test]
fn message_code_into_u8_matches_as_u8() {
    for &code in MessageCode::all() {
        let converted: u8 = code.into();
        assert_eq!(converted, code.as_u8());
    }
}

#[test]
fn message_code_from_u8_matches_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        assert_eq!(MessageCode::from_u8(raw), Some(code));
        assert_eq!(MessageCode::try_from(raw).ok(), MessageCode::from_u8(raw));
    }
}

#[test]
fn message_code_from_u8_rejects_unknown_values() {
    assert_eq!(MessageCode::from_u8(11), None);
    assert_eq!(MessageCode::from_u8(0xFF), None);
}

#[test]
fn message_code_from_str_parses_known_names() {
    for &code in MessageCode::all() {
        let parsed: MessageCode = code.name().parse().expect("known name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn message_code_from_str_rejects_unknown_names() {
    let err = "MSG_SOMETHING_ELSE".parse::<MessageCode>().unwrap_err();
    assert_eq!(err.invalid_name(), "MSG_SOMETHING_ELSE");
    assert_eq!(
        err.to_string(),
        "unknown multiplexed message code name: \"MSG_SOMETHING_ELSE\""
    );
}

#[test]
fn message_code_all_is_sorted_by_numeric_value() {
    let all = MessageCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "MessageCode::all() is not sorted: {:?}",
            all
        );
    }
}

#[test]
fn header_round_trips_for_all_codes_and_sample_lengths() {
    const PAYLOAD_SAMPLES: [u32; 3] = [0, 1, MAX_PAYLOAD_LENGTH];

    for &code in MessageCode::all() {
        for &len in &PAYLOAD_SAMPLES {
            let header = MessageHeader::new(code, len).expect("constructible header");
            let encoded = header.encode();
            let decoded = MessageHeader::decode(&encoded).expect("decode succeeds");
            assert_eq!(decoded.code(), code);
            assert_eq!(decoded.payload_len(), len);
        }
    }
}

#[test]
fn logging_classification_matches_upstream_set() {
    const LOGGING_CODES: &[MessageCode] = &[
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::ErrorSocket,
        MessageCode::ErrorUtf8,
        MessageCode::Log,
        MessageCode::Client,
    ];

    for &code in MessageCode::all() {
        let expected = LOGGING_CODES.contains(&code);
        assert_eq!(code.is_logging(), expected, "mismatch for code {code:?}");
    }
}

#[test]
fn message_code_name_matches_upstream_identifiers() {
    use MessageCode::*;

    let expected = [
        (Data, "MSG_DATA"),
        (ErrorXfer, "MSG_ERROR_XFER"),
        (Info, "MSG_INFO"),
        (Error, "MSG_ERROR"),
        (Warning, "MSG_WARNING"),
        (ErrorSocket, "MSG_ERROR_SOCKET"),
        (Log, "MSG_LOG"),
        (Client, "MSG_CLIENT"),
        (ErrorUtf8, "MSG_ERROR_UTF8"),
        (Redo, "MSG_REDO"),
        (Stats, "MSG_STATS"),
        (IoError, "MSG_IO_ERROR"),
        (IoTimeout, "MSG_IO_TIMEOUT"),
        (NoOp, "MSG_NOOP"),
        (ErrorExit, "MSG_ERROR_EXIT"),
        (Success, "MSG_SUCCESS"),
        (Deleted, "MSG_DELETED"),
        (NoSend, "MSG_NO_SEND"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}

#[test]
fn message_code_flush_alias_matches_info() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());

    let parsed: MessageCode = "MSG_FLUSH".parse().expect("known alias");
    assert_eq!(parsed, MessageCode::Info);
}

proptest! {
    #[test]
    fn prop_header_round_trips_for_random_inputs(
        code in proptest::sample::select(&MessageCode::ALL),
        payload in 0u32..=MAX_PAYLOAD_LENGTH,
    ) {
        let header = MessageHeader::new(code, payload).expect("constructible header");
        let encoded = header.encode();
        let decoded = MessageHeader::decode(&encoded).expect("decode succeeds");
        prop_assert_eq!(decoded, header);
    }

    #[test]
    fn prop_decode_rejects_all_truncated_header_lengths(len in 0usize..HEADER_LEN) {
        let bytes = vec![0u8; len];
        let err = MessageHeader::decode(&bytes).unwrap_err();
        prop_assert_eq!(err, EnvelopeError::TruncatedHeader { actual: len });
    }

    #[test]
    fn prop_decode_rejects_invalid_tag_prefixes(
        tag in 0u8..MPLEX_BASE,
        payload in 0u32..=PAYLOAD_MASK,
    ) {
        let raw = (u32::from(tag) << 24) | (payload & PAYLOAD_MASK);
        let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
        prop_assert_eq!(err, EnvelopeError::InvalidTag(tag));
    }

    #[test]
    fn prop_message_header_new_rejects_oversized_payloads(
        len in (MAX_PAYLOAD_LENGTH + 1)..=(MAX_PAYLOAD_LENGTH + 0x0FFF),
    ) {
        let err = MessageHeader::new(MessageCode::Info, len).unwrap_err();
        prop_assert_eq!(err, EnvelopeError::OversizedPayload(len));
    }

    #[test]
    fn prop_decode_rejects_unknown_message_codes(
        code in 0u8..=(u8::MAX - MPLEX_BASE),
        payload in 0u32..=PAYLOAD_MASK,
    ) {
        prop_assume!(MessageCode::from_u8(code).is_none());
        let tag = u32::from(MPLEX_BASE) + u32::from(code);
        let raw = (tag << 24) | (payload & PAYLOAD_MASK);
        let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
        prop_assert_eq!(err, EnvelopeError::UnknownMessageCode(code));
    }
}

#[test]
fn log_code_all_is_sorted_by_numeric_value() {
    let all = LogCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "LogCode::all() unsorted: {all:?}"
        );
    }
}

#[test]
fn log_code_from_u8_matches_try_from() {
    for &code in LogCode::all() {
        let raw = code.as_u8();
        assert_eq!(LogCode::from_u8(raw), Some(code));
        assert_eq!(LogCode::try_from(raw).ok(), LogCode::from_u8(raw));
    }
}

#[test]
fn log_code_from_u8_rejects_unknown_values() {
    assert_eq!(LogCode::from_u8(9), None);
    let err = LogCode::try_from(9).unwrap_err();
    assert_eq!(err.invalid_value(), Some(9));
    assert_eq!(err.to_string(), "unknown log code value: 9");
}

#[test]
fn log_code_from_str_parses_known_names() {
    for &code in LogCode::all() {
        let parsed: LogCode = code.name().parse().expect("known log code name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn log_code_from_str_rejects_unknown_names() {
    let err = "FUNKNOWN".parse::<LogCode>().unwrap_err();
    assert_eq!(err.invalid_name(), Some("FUNKNOWN"));
    assert_eq!(err.to_string(), "unknown log code name: \"FUNKNOWN\"");
    assert_eq!(err.invalid_value(), None);
}

#[test]
fn log_code_name_matches_upstream_identifiers() {
    use LogCode::*;

    let expected = [
        (None, "FNONE"),
        (ErrorXfer, "FERROR_XFER"),
        (Info, "FINFO"),
        (Error, "FERROR"),
        (Warning, "FWARNING"),
        (ErrorSocket, "FERROR_SOCKET"),
        (Log, "FLOG"),
        (Client, "FCLIENT"),
        (ErrorUtf8, "FERROR_UTF8"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}

#[test]
fn message_code_log_code_matches_logging_subset() {
    for &code in MessageCode::all() {
        let log_code = code.log_code();
        assert_eq!(
            log_code.is_some(),
            code.is_logging(),
            "mismatch for {code:?}"
        );

        if let Some(mapped) = log_code {
            assert!(matches!(
                mapped,
                LogCode::ErrorXfer
                    | LogCode::Info
                    | LogCode::Error
                    | LogCode::Warning
                    | LogCode::ErrorSocket
                    | LogCode::Log
                    | LogCode::Client
                    | LogCode::ErrorUtf8
            ));
        }
    }
}

#[test]
fn message_code_from_log_code_round_trips_logging_variants() {
    for &log in LogCode::all() {
        match MessageCode::from_log_code(log) {
            Some(code) => {
                assert_eq!(code.log_code(), Some(log), "round-trip failed for {log:?}");
            }
            None => assert_eq!(log, LogCode::None, "only FNONE lacks a multiplexed tag"),
        }
    }
}

#[test]
fn message_code_from_log_code_rejects_none_variant() {
    assert_eq!(MessageCode::from_log_code(LogCode::None), None);
}

#[test]
fn try_from_log_code_maps_logging_variants() {
    for &log in LogCode::all() {
        match MessageCode::try_from(log) {
            Ok(code) => assert_eq!(code.log_code(), Some(log)),
            Err(err) => {
                assert_eq!(log, LogCode::None);
                assert_eq!(err.log_code(), Some(log));
                assert!(err.message_code().is_none());
                assert_eq!(
                    err.to_string(),
                    "log code FNONE has no multiplexed message equivalent"
                );
            }
        }
    }
}

#[test]
fn try_from_message_code_requires_logging_equivalent() {
    for &code in MessageCode::all() {
        match LogCode::try_from(code) {
            Ok(log) => assert_eq!(MessageCode::from_log_code(log), Some(code)),
            Err(err) => {
                assert!(code.log_code().is_none());
                assert_eq!(err.message_code(), Some(code));
                assert!(err.log_code().is_none());
                assert_eq!(
                    err.to_string(),
                    format!("message code {code} has no log code equivalent")
                );
            }
        }
    }
}
