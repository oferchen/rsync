use super::*;

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
