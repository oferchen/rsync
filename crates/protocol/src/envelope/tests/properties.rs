use super::*;
use proptest::prelude::*;

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
