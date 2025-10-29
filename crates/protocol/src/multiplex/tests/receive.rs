use super::*;
use std::io::{self, Cursor};

#[test]
fn send_and_receive_round_trip_info_message() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Info, b"hello world").expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"hello world");
    assert_eq!(frame.payload_len(), b"hello world".len());
}

#[test]
fn round_trip_zero_length_payload() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Warning, b"").expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Warning);
    assert!(frame.payload().is_empty());
    assert_eq!(frame.payload_len(), 0);
}

#[test]
fn recv_msg_reports_truncated_payload() {
    let header = MessageHeader::new(MessageCode::Warning, 4)
        .expect("header")
        .encode();
    let mut buffer = header.to_vec();
    buffer.extend_from_slice(&[1, 2]);

    let err = recv_msg(&mut Cursor::new(buffer)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        err.to_string(),
        "multiplexed payload truncated: expected 4 bytes but received 2"
    );
}

#[test]
fn recv_msg_into_truncates_buffer_after_short_payload() {
    let header = MessageHeader::new(MessageCode::Client, 4)
        .expect("header")
        .encode();
    let mut data = header.to_vec();
    data.extend_from_slice(&[1, 2]);

    let mut cursor = Cursor::new(data);
    let mut buffer = vec![0xAA, 0xBB, 0xCC];
    let err = recv_msg_into(&mut cursor, &mut buffer).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        err.to_string(),
        "multiplexed payload truncated: expected 4 bytes but received 2"
    );
    assert_eq!(buffer, vec![1, 2]);
}

#[test]
fn recv_msg_reports_truncated_header() {
    let mut cursor = Cursor::new([0u8; HEADER_LEN - 1]);
    let err = recv_msg(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn recv_msg_rejects_unknown_message_codes() {
    let unknown_code = 11u8;
    let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code);
    let raw = (tag << 24).to_le_bytes();
    let err = recv_msg(&mut Cursor::new(raw)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn recv_msg_rejects_headers_without_mplex_base() {
    let tag_without_base = u32::from(MPLEX_BASE - 1) << 24;
    let err = recv_msg(&mut Cursor::new(tag_without_base.to_le_bytes())).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string()
            .contains("multiplexed header contained invalid tag byte")
    );
}

#[test]
fn recv_msg_into_populates_existing_buffer() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Client, b"payload").expect("send succeeds");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Client);
    assert_eq!(buffer.as_slice(), b"payload");
}

#[test]
fn recv_msg_into_reuses_buffer_capacity_for_smaller_payloads() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Warning, b"hi").expect("send succeeds");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::with_capacity(64);
    buffer.extend_from_slice(&[0u8; 16]);
    let capacity_before = buffer.capacity();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(buffer.as_slice(), b"hi");
    assert_eq!(buffer.capacity(), capacity_before);
}

#[test]
fn recv_msg_into_handles_back_to_back_frames_without_reallocation() {
    let mut stream = Vec::new();
    let first_payload = b"primary payload";
    let second_payload = b"ok";

    send_msg(&mut stream, MessageCode::Info, first_payload).expect("first send");
    send_msg(&mut stream, MessageCode::Warning, second_payload).expect("second send");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::new();

    let first_code = recv_msg_into(&mut cursor, &mut buffer).expect("first receive");
    assert_eq!(first_code, MessageCode::Info);
    assert_eq!(buffer.as_slice(), first_payload);

    let capacity_after_first = buffer.capacity();
    let ptr_after_first = buffer.as_ptr();

    let second_code = recv_msg_into(&mut cursor, &mut buffer).expect("second receive");
    assert_eq!(second_code, MessageCode::Warning);
    assert_eq!(buffer.as_slice(), second_payload);
    assert_eq!(buffer.capacity(), capacity_after_first);
    assert_eq!(buffer.as_ptr(), ptr_after_first);
}

#[test]
fn recv_msg_into_clears_buffer_for_empty_payloads() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Log, b"").expect("send succeeds");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::with_capacity(8);
    buffer.extend_from_slice(b"junk");
    let capacity_before = buffer.capacity();
    let ptr_before = buffer.as_ptr();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Log);
    assert!(buffer.is_empty());
    assert_eq!(buffer.capacity(), capacity_before);
    assert_eq!(buffer.as_ptr(), ptr_before);
}

#[test]
fn recv_msg_into_populates_caller_buffer() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Warning, b"payload").expect("send succeeds");

    let mut cursor = Cursor::new(serialized);
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Warning);
    assert_eq!(buffer, b"payload");
}

#[test]
fn recv_msg_into_reuses_existing_capacity() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Info, b"hello").expect("send succeeds");

    let mut cursor = Cursor::new(serialized);
    let mut buffer = vec![0u8; 8];
    let original_capacity = buffer.capacity();
    let original_ptr = buffer.as_ptr();
    buffer.clear();

    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Info);
    assert_eq!(buffer, b"hello");
    assert_eq!(buffer.capacity(), original_capacity);
    assert_eq!(buffer.as_ptr(), original_ptr);
}

#[test]
fn recv_msg_into_handles_empty_payload_without_reading() {
    let mut serialized = Vec::new();
    send_msg(&mut serialized, MessageCode::Log, b"").expect("send succeeds");

    let mut cursor = Cursor::new(serialized);
    let mut buffer = vec![1u8; 4];
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Log);
    assert!(buffer.is_empty());
}
