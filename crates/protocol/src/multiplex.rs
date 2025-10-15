use std::io::{self, Read, Write};

use crate::envelope::{EnvelopeError, HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader};

/// A decoded multiplexed message consisting of the tag and payload bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageFrame {
    code: MessageCode,
    payload: Vec<u8>,
}

impl MessageFrame {
    /// Constructs a frame from a message code and owned payload bytes.
    pub fn new(code: MessageCode, payload: Vec<u8>) -> Result<Self, io::Error> {
        let len = payload.len();
        if len > MAX_PAYLOAD_LENGTH as usize {
            return Err(invalid_len_error(len));
        }

        let len = u32::try_from(len).expect("checked against MAX_PAYLOAD_LENGTH");
        MessageHeader::new(code, len).map_err(map_envelope_error_for_input)?;
        Ok(Self { code, payload })
    }

    /// Returns the message code associated with the frame.
    #[must_use]
    pub const fn code(&self) -> MessageCode {
        self.code
    }

    /// Returns the raw payload bytes carried by the frame.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consumes the frame and returns the owned payload bytes.
    #[must_use]
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }
}

/// Sends a multiplexed message to `writer` using the upstream rsync envelope format.
///
/// The payload length is validated against [`MAX_PAYLOAD_LENGTH`], mirroring the
/// 24-bit limit imposed by the C implementation. Violations result in
/// [`io::ErrorKind::InvalidInput`].
pub fn send_msg<W: Write>(writer: &mut W, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    let len = payload.len();
    if len > MAX_PAYLOAD_LENGTH as usize {
        return Err(invalid_len_error(len));
    }

    let len = u32::try_from(len).expect("checked against MAX_PAYLOAD_LENGTH");
    let header = MessageHeader::new(code, len).map_err(map_envelope_error_for_input)?;
    writer.write_all(&header.encode())?;
    writer.write_all(payload)?;
    Ok(())
}

/// Receives the next multiplexed message from `reader`.
///
/// The function blocks until the full header and payload are read or an I/O
/// error occurs. Invalid headers surface as [`io::ErrorKind::InvalidData`].
pub fn recv_msg<R: Read>(reader: &mut R) -> io::Result<MessageFrame> {
    let mut header_bytes = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes)?;
    let header = MessageHeader::decode(&header_bytes).map_err(map_envelope_error)?;
    let len = header.payload_len() as usize;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;

    Ok(MessageFrame {
        code: header.code(),
        payload,
    })
}

fn map_envelope_error(err: EnvelopeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

fn map_envelope_error_for_input(err: EnvelopeError) -> io::Error {
    match err {
        EnvelopeError::OversizedPayload(_) => io::Error::new(io::ErrorKind::InvalidInput, err),
        other => map_envelope_error(other),
    }
}

fn invalid_len_error(len: usize) -> io::Error {
    let len = len as u128;
    let max = u128::from(MAX_PAYLOAD_LENGTH);
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("multiplexed payload length {len} exceeds maximum {max}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::MAX_PAYLOAD_LENGTH;

    #[test]
    fn send_and_receive_round_trip_info_message() {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Info, b"hello world").expect("send succeeds");

        let mut cursor = io::Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), b"hello world");
    }

    #[test]
    fn recv_msg_reports_truncated_payload() {
        let header = MessageHeader::new(MessageCode::Warning, 4)
            .expect("header")
            .encode();
        let mut buffer = header.to_vec();
        buffer.extend_from_slice(&[1, 2]);

        let err = recv_msg(&mut io::Cursor::new(buffer)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_msg_rejects_unknown_message_codes() {
        let unknown_code = 11u8;
        let tag = u32::from(7u8) + u32::from(unknown_code); // MPLEX_BASE + unknown code
        let raw = (tag << 24).to_le_bytes();
        let err = recv_msg(&mut io::Cursor::new(raw)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
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
}
