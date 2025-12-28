use super::support::encode_frame;
use super::*;
use std::convert::TryFrom as _;
use std::io;

#[test]
fn decode_from_slice_round_trips_and_exposes_remainder() {
    let first = encode_frame(MessageCode::Info, b"hello");
    let second = encode_frame(MessageCode::Error, b"world");

    let mut concatenated = first;
    concatenated.extend_from_slice(&second);

    let (frame, remainder) =
        MessageFrame::decode_from_slice(&concatenated).expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"hello");
    assert_eq!(remainder, second.as_slice());
}

#[test]
fn decode_from_slice_errors_for_truncated_header() {
    let err = MessageFrame::decode_from_slice(&[0x01, 0x02]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn decode_from_slice_errors_for_truncated_payload() {
    let header = MessageHeader::new(MessageCode::Data, 4).expect("constructible header");
    let mut bytes = Vec::from(header.encode());
    bytes.extend_from_slice(&[0xAA, 0xBB]);

    let err = MessageFrame::decode_from_slice(&bytes).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn message_frame_try_from_slice_round_trips_single_frame() {
    let encoded = encode_frame(MessageCode::Warning, b"payload");
    let frame = MessageFrame::try_from(encoded.as_slice()).expect("decode succeeds");

    assert_eq!(frame.code(), MessageCode::Warning);
    assert_eq!(frame.payload(), b"payload");
}

#[test]
fn message_frame_try_from_slice_rejects_trailing_bytes() {
    let frame = encode_frame(MessageCode::Stats, &[0x01, 0x02, 0x03, 0x04]);
    let mut bytes = frame;
    bytes.extend_from_slice(&[0xFF, 0xEE]);

    let err = MessageFrame::try_from(bytes.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "input slice contains 2 trailing bytes after multiplexed frame"
    );
}

#[test]
fn message_frame_header_reflects_current_payload_length() {
    let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec()).expect("frame");
    let header = frame.header().expect("header recomputation succeeds");

    assert_eq!(header.code(), MessageCode::Info);
    assert_eq!(header.payload_len(), 3);
}

#[test]
fn message_frame_header_detects_payload_growth_past_limit() {
    let mut frame = MessageFrame::new(MessageCode::Data, Vec::new()).expect("frame");
    frame.payload = vec![0u8; MAX_PAYLOAD_LENGTH as usize + 1];

    let err = frame.header().expect_err("oversized payload must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("multiplexed payload length"),
        "error message should mention the payload limit"
    );
}

#[test]
fn encode_into_writer_matches_send_frame() {
    let frame = MessageFrame::new(MessageCode::Info, b"payload".to_vec()).expect("frame is valid");

    let mut via_method = Vec::new();
    frame
        .encode_into_writer(&mut via_method)
        .expect("method succeeds");

    let mut via_function = Vec::new();
    send_frame(&mut via_function, &frame).expect("send_frame succeeds");

    assert_eq!(via_method, via_function);
}

#[test]
fn encode_into_writer_handles_empty_payloads() {
    let frame = MessageFrame::new(MessageCode::Error, Vec::new()).expect("frame is valid");

    let mut buffer = Vec::new();
    frame
        .encode_into_writer(&mut buffer)
        .expect("method succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);
    assert_eq!(
        &buffer[..HEADER_LEN],
        &MessageHeader::new(frame.code(), 0).unwrap().encode()
    );
}

#[test]
fn encode_into_vec_appends_multiplexed_bytes() {
    let payload = b"encoded payload".to_vec();
    let frame = MessageFrame::new(MessageCode::Info, payload).expect("frame is valid");

    let mut via_encode = vec![0xAA];
    frame
        .encode_into_vec(&mut via_encode)
        .expect("encode_into_vec succeeds");

    let mut via_send = vec![0xAA];
    send_frame(&mut via_send, &frame).expect("send_frame succeeds");

    assert_eq!(via_encode, via_send);
}

#[test]
fn message_frame_into_parts_returns_code_and_payload_without_clone() {
    let frame = MessageFrame::new(MessageCode::Warning, b"payload".to_vec()).expect("frame");
    let (code, payload) = frame.into_parts();

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(payload, b"payload");
}

#[test]
fn message_frame_into_payload_returns_owned_payload() {
    let payload = b"payload".to_vec();
    let frame = MessageFrame::new(MessageCode::Warning, payload.clone()).expect("frame");

    let owned = frame.into_payload();

    assert_eq!(owned, payload);
}

#[test]
fn message_frame_payload_mut_allows_in_place_updates() {
    let mut frame = MessageFrame::new(MessageCode::Data, b"payload".to_vec()).expect("frame");

    {
        let payload = frame.payload_mut();
        payload[..4].copy_from_slice(b"data");
    }

    assert_eq!(frame.payload(), b"dataoad");
    assert_eq!(frame.payload_len(), 7);
}

#[test]
fn message_frame_as_ref_exposes_payload_slice() {
    let frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
    assert_eq!(AsRef::<[u8]>::as_ref(&frame), b"slice");
    let deref: &[u8] = &frame;
    assert_eq!(deref, b"slice");
}

#[test]
fn message_frame_as_mut_allows_mutating_payload_slice() {
    let mut frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
    let payload = AsMut::<[u8]>::as_mut(&mut frame);
    payload.copy_from_slice(b"PATCH");
    assert_eq!(frame.payload(), b"PATCH");
    {
        let deref: &mut [u8] = &mut frame;
        deref.copy_from_slice(b"lower");
    }
    assert_eq!(frame.payload(), b"lower");
}

#[test]
fn message_frame_new_validates_payload_length() {
    let frame = MessageFrame::new(MessageCode::Stats, b"stats".to_vec()).expect("frame");
    assert_eq!(frame.code(), MessageCode::Stats);
    assert_eq!(frame.payload(), b"stats");
}

#[test]
fn message_frame_new_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = MessageFrame::new(MessageCode::Info, payload).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "multiplexed payload length {} exceeds maximum {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1,
            u128::from(MAX_PAYLOAD_LENGTH)
        )
    );
}

#[test]
fn message_frame_try_from_tuple_constructs_frame() {
    let payload = b"payload".to_vec();
    let frame = MessageFrame::try_from((MessageCode::Warning, payload.clone()))
        .expect("tuple conversion succeeds");
    assert_eq!(frame.code(), MessageCode::Warning);
    assert_eq!(frame.payload(), payload);
}

#[test]
fn message_frame_try_from_tuple_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = MessageFrame::try_from((MessageCode::Info, payload)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains(&format!(
        "multiplexed payload length {}",
        u128::from(MAX_PAYLOAD_LENGTH) + 1
    )));
}

#[test]
fn message_frame_into_tuple_returns_owned_parts() {
    let frame = MessageFrame::new(MessageCode::Deleted, b"done".to_vec()).expect("frame");
    let (code, payload): (MessageCode, Vec<u8>) = frame.into();
    assert_eq!(code, MessageCode::Deleted);
    assert_eq!(payload, b"done");
}
