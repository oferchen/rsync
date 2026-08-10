use super::*;
use std::io::{self, Write};

#[test]
fn send_msg_emits_upstream_compatible_envelope_for_common_tags() {
    struct Sample {
        code: MessageCode,
        payload: &'static [u8],
    }

    let samples = [
        Sample {
            code: MessageCode::Info,
            payload: b"abc",
        },
        Sample {
            code: MessageCode::Error,
            payload: b"",
        },
        Sample {
            code: MessageCode::Stats,
            payload: &[0x01, 0x02, 0x03, 0x04],
        },
    ];

    for sample in samples {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, sample.code, sample.payload).expect("send succeeds");

        assert_eq!(buffer.len(), HEADER_LEN + sample.payload.len());

        let tag = u32::from(MPLEX_BASE) + u32::from(sample.code.as_u8());
        let expected_header = ((tag << 24) | sample.payload.len() as u32).to_le_bytes();

        assert_eq!(&buffer[..HEADER_LEN], &expected_header);
        assert_eq!(&buffer[HEADER_LEN..], sample.payload);
    }
}

#[test]
fn send_frame_matches_send_msg_encoding() {
    let payload = b"payload bytes".to_vec();
    let frame = MessageFrame::new(MessageCode::Info, payload).expect("frame is valid");

    let mut via_send_msg = Vec::new();
    send_msg(&mut via_send_msg, frame.code(), frame.payload()).expect("send_msg succeeds");

    let mut via_send_frame = Vec::new();
    send_frame(&mut via_send_frame, &frame).expect("send_frame succeeds");

    assert_eq!(via_send_frame, via_send_msg);
}

#[test]
fn send_frame_handles_empty_payloads() {
    let frame = MessageFrame::new(MessageCode::Error, Vec::new()).expect("frame is valid");

    let mut buffer = Vec::new();
    send_frame(&mut buffer, &frame).expect("send_frame succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);
    assert_eq!(
        &buffer[..HEADER_LEN],
        &MessageHeader::new(frame.code(), 0).unwrap().encode()
    );
}

#[test]
fn send_msg_rejects_oversized_payload() {
    let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = send_msg(&mut io::sink(), MessageCode::Error, &payload).unwrap_err();
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
fn send_msg_propagates_write_errors() {
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = FailingWriter;
    let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(err.to_string().contains("injected failure"));
}
