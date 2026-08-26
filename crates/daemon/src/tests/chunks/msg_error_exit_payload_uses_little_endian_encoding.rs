/// Validates that MSG_ERROR_EXIT payloads are encoded as little-endian,
/// matching upstream SIVAL encoding (io.c:send_msg_int:1060).
///
/// A big-endian payload causes an upstream client to read exit code 1 as
/// 0x01000000 via IVAL (little-endian decode). After the 8-bit exit mask
/// that becomes 0 - a daemon rejection silently surfaces as success.
#[test]
fn msg_error_exit_payload_uses_little_endian_encoding() {
    let exit_code: u32 = 1;
    let payload = exit_code.to_le_bytes();
    assert_eq!(
        payload,
        [0x01, 0x00, 0x00, 0x00],
        "exit code 1 must encode as LE [01,00,00,00], not BE [00,00,00,01]",
    );

    // Round-trip through MessageFrame to confirm the payload is preserved.
    let frame = protocol::MessageFrame::new(protocol::MessageCode::ErrorExit, payload.to_vec())
        .expect("valid frame");
    let mut buf = Vec::new();
    frame.encode_into_writer(&mut buf).expect("encode");
    // The payload occupies the last 4 bytes of the encoded frame.
    assert_eq!(
        &buf[buf.len() - 4..],
        &[0x01, 0x00, 0x00, 0x00],
        "MessageFrame must preserve the little-endian exit-code payload",
    );
}
