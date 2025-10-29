use super::support::encode_frame;
use super::*;
use std::convert::TryFrom as _;
use std::io;

#[test]
fn borrowed_message_frame_decodes_without_allocating_payload() {
    let first = encode_frame(MessageCode::Data, b"abcde");
    let second = encode_frame(MessageCode::Info, b"more");

    let mut concatenated = first.clone();
    concatenated.extend_from_slice(&second);

    let (frame, remainder) =
        BorrowedMessageFrame::decode_from_slice(&concatenated).expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload(), b"abcde");
    assert_eq!(remainder, second.as_slice());

    let owned = frame.into_owned().expect("conversion succeeds");
    assert_eq!(owned.code(), MessageCode::Data);
    assert_eq!(owned.payload(), b"abcde");
}

#[test]
fn borrowed_message_frame_matches_owned_decoding() {
    let encoded = encode_frame(MessageCode::Warning, b"payload");

    let (borrowed, remainder) =
        BorrowedMessageFrame::decode_from_slice(&encoded).expect("decode succeeds");
    assert!(remainder.is_empty());

    let owned = MessageFrame::try_from(encoded.as_slice()).expect("owned decode succeeds");
    assert_eq!(borrowed.code(), owned.code());
    assert_eq!(borrowed.payload(), owned.payload());

    let borrowed_exact =
        BorrowedMessageFrame::try_from(encoded.as_slice()).expect("borrowed decode succeeds");
    assert_eq!(borrowed_exact.code(), owned.code());
    assert_eq!(borrowed_exact.payload(), owned.payload());
}

#[test]
fn borrowed_message_frame_try_from_rejects_trailing_bytes() {
    let frame = encode_frame(MessageCode::Info, b"hello");
    let mut bytes = frame.clone();
    bytes.push(0xAA);

    let err = BorrowedMessageFrame::try_from(bytes.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "input slice contains 1 trailing byte after multiplexed frame"
    );
}

#[test]
fn borrowed_message_frames_iterates_over_sequence() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&encode_frame(MessageCode::Info, b"abc"));
    bytes.extend_from_slice(&encode_frame(MessageCode::Warning, b""));

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let first = iter
        .next()
        .expect("first frame present")
        .expect("decode succeeds");
    assert_eq!(first.code(), MessageCode::Info);
    assert_eq!(first.payload(), b"abc");

    let second = iter
        .next()
        .expect("second frame present")
        .expect("decode succeeds");
    assert_eq!(second.code(), MessageCode::Warning);
    assert!(second.payload().is_empty());

    assert!(iter.next().is_none());
    assert!(iter.remainder().is_empty());
}

#[test]
fn borrowed_message_frames_reports_decode_error() {
    let mut bytes = encode_frame(MessageCode::Info, b"abc");
    let mut truncated = encode_frame(MessageCode::Error, b"payload");
    truncated.pop();
    bytes.extend_from_slice(&truncated);

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let first = iter
        .next()
        .expect("first frame present")
        .expect("decode succeeds");
    assert_eq!(first.code(), MessageCode::Info);

    let err = iter
        .next()
        .expect("error surfaced")
        .expect_err("decode should fail due to truncation");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(iter.next().is_none());
    assert_eq!(iter.remainder(), truncated.as_slice());
}

#[test]
fn borrowed_message_frames_reports_trailing_bytes() {
    let mut bytes = encode_frame(MessageCode::Info, b"abc");
    bytes.push(0xAA);

    let mut iter = BorrowedMessageFrames::new(&bytes);

    let frame = iter
        .next()
        .expect("frame present")
        .expect("decode succeeds");
    assert_eq!(frame.code(), MessageCode::Info);

    let err = iter
        .next()
        .expect("error returned")
        .expect_err("trailing byte should be reported");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(iter.next().is_none());
    assert_eq!(iter.remainder(), &[0xAA][..]);
}
