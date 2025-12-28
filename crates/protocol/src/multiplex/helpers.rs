use std::collections::TryReserveError;
use std::io::{self, Read};

use crate::envelope::{EnvelopeError, HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageHeader};

#[cold]
pub(super) fn map_envelope_error(err: EnvelopeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

#[cold]
pub(super) fn map_envelope_error_for_input(err: EnvelopeError) -> io::Error {
    match err {
        EnvelopeError::OversizedPayload(_) => io::Error::new(io::ErrorKind::InvalidInput, err),
        other => map_envelope_error(other),
    }
}

#[cold]
fn invalid_len_error(len: usize) -> io::Error {
    let len = len as u128;
    let max = u128::from(MAX_PAYLOAD_LENGTH);
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("multiplexed payload length {len} exceeds maximum {max}"),
    )
}

#[cold]
pub(super) fn truncated_frame_error(expected: usize, actual: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("multiplexed frame truncated: expected {expected} bytes but received {actual}"),
    )
}

#[cold]
pub(super) fn truncated_payload_error(expected: usize, actual: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("multiplexed payload truncated: expected {expected} bytes but received {actual}"),
    )
}

#[cold]
pub(super) fn trailing_frame_data_error(trailing: usize) -> io::Error {
    let unit = if trailing == 1 { "byte" } else { "bytes" };
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("input slice contains {trailing} trailing {unit} after multiplexed frame"),
    )
}

pub(super) fn ensure_payload_length(len: usize) -> io::Result<u32> {
    if len > MAX_PAYLOAD_LENGTH as usize {
        return Err(invalid_len_error(len));
    }

    Ok(len as u32)
}

pub(super) fn reserve_payload(buffer: &mut Vec<u8>, len: usize) -> io::Result<()> {
    if buffer.capacity() < len {
        let additional = len.saturating_sub(buffer.len());
        debug_assert!(
            additional > 0,
            "reserve_payload called without additional elements",
        );
        buffer
            .try_reserve_exact(additional)
            .map_err(map_allocation_error)?;
    }

    Ok(())
}

pub(super) fn read_payload<R: Read>(reader: &mut R, len: usize) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    read_payload_into(reader, &mut payload, len)?;
    Ok(payload)
}

pub(super) fn read_payload_into<R: Read>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    len: usize,
) -> io::Result<()> {
    buffer.clear();

    if len == 0 {
        return Ok(());
    }

    reserve_payload(buffer, len)?;

    buffer.resize(len, 0);

    let mut read_total = 0;
    while read_total < len {
        match reader.read(&mut buffer[read_total..]) {
            Ok(0) => {
                buffer.truncate(read_total);
                return Err(truncated_payload_error(len, read_total));
            }
            Ok(bytes_read) => {
                read_total += bytes_read;
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                buffer.truncate(read_total);
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    return Err(truncated_payload_error(len, read_total));
                }
                return Err(err);
            }
        }
    }

    Ok(())
}

#[cold]
pub(super) fn map_allocation_error(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, err)
}

pub(super) fn decode_frame_parts(bytes: &[u8]) -> io::Result<(MessageHeader, &[u8], &[u8])> {
    if bytes.len() < HEADER_LEN {
        return Err(truncated_frame_error(HEADER_LEN, bytes.len()));
    }

    let header = MessageHeader::decode(&bytes[..HEADER_LEN]).map_err(map_envelope_error)?;
    let payload_len = header.payload_len_usize();
    let frame_len = HEADER_LEN + payload_len;

    if bytes.len() < frame_len {
        return Err(truncated_frame_error(frame_len, bytes.len()));
    }

    let payload = &bytes[HEADER_LEN..frame_len];
    let remainder = &bytes[frame_len..];

    Ok((header, payload, remainder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageCode;
    use std::io::Cursor;

    #[test]
    fn map_envelope_error_returns_invalid_data() {
        let err = EnvelopeError::InvalidTag(5);
        let io_err = map_envelope_error(err);
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn map_envelope_error_for_input_oversized_returns_invalid_input() {
        let err = EnvelopeError::OversizedPayload(MAX_PAYLOAD_LENGTH + 1);
        let io_err = map_envelope_error_for_input(err);
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn map_envelope_error_for_input_other_returns_invalid_data() {
        let err = EnvelopeError::InvalidTag(5);
        let io_err = map_envelope_error_for_input(err);
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_frame_error_creates_unexpected_eof() {
        let err = truncated_frame_error(100, 50);
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let msg = err.to_string();
        assert!(msg.contains("100"));
        assert!(msg.contains("50"));
    }

    #[test]
    fn truncated_payload_error_creates_unexpected_eof() {
        let err = truncated_payload_error(100, 50);
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let msg = err.to_string();
        assert!(msg.contains("payload"));
    }

    #[test]
    fn trailing_frame_data_error_singular_byte() {
        let err = trailing_frame_data_error(1);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("1 trailing byte"));
    }

    #[test]
    fn trailing_frame_data_error_plural_bytes() {
        let err = trailing_frame_data_error(5);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("5 trailing bytes"));
    }

    #[test]
    fn ensure_payload_length_accepts_valid() {
        let result = ensure_payload_length(1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1000);
    }

    #[test]
    fn ensure_payload_length_accepts_zero() {
        let result = ensure_payload_length(0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn ensure_payload_length_accepts_max() {
        let result = ensure_payload_length(MAX_PAYLOAD_LENGTH as usize);
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_payload_length_rejects_oversized() {
        let result = ensure_payload_length(MAX_PAYLOAD_LENGTH as usize + 1);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn reserve_payload_succeeds_for_small_buffer() {
        let mut buffer = Vec::new();
        let result = reserve_payload(&mut buffer, 100);
        assert!(result.is_ok());
        assert!(buffer.capacity() >= 100);
    }

    #[test]
    fn reserve_payload_noop_when_sufficient_capacity() {
        let mut buffer = Vec::with_capacity(200);
        let result = reserve_payload(&mut buffer, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn read_payload_reads_exact_bytes() {
        let data = b"hello world";
        let mut reader = Cursor::new(data.as_slice());
        let result = read_payload(&mut reader, 5);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"hello");
    }

    #[test]
    fn read_payload_empty() {
        let data: &[u8] = &[];
        let mut reader = Cursor::new(data);
        let result = read_payload(&mut reader, 0);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn read_payload_truncated() {
        let data = b"short";
        let mut reader = Cursor::new(data.as_slice());
        let result = read_payload(&mut reader, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_payload_into_clears_buffer() {
        let mut buffer = vec![1, 2, 3, 4, 5];
        let data = b"abc";
        let mut reader = Cursor::new(data.as_slice());
        let result = read_payload_into(&mut reader, &mut buffer, 3);
        assert!(result.is_ok());
        assert_eq!(buffer, b"abc");
    }

    #[test]
    fn decode_frame_parts_truncated_header() {
        let bytes = [0u8, 1, 2]; // Only 3 bytes, need 4
        let result = decode_frame_parts(&bytes);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_frame_parts_valid_frame() {
        let header = MessageHeader::new(MessageCode::Info, 3).unwrap();
        let mut bytes = Vec::from(header.encode());
        bytes.extend_from_slice(b"abc");
        let result = decode_frame_parts(&bytes);
        assert!(result.is_ok());
        let (decoded_header, payload, remainder) = result.unwrap();
        assert_eq!(decoded_header.code(), MessageCode::Info);
        assert_eq!(payload, b"abc");
        assert!(remainder.is_empty());
    }

    #[test]
    fn decode_frame_parts_with_remainder() {
        let header = MessageHeader::new(MessageCode::Info, 3).unwrap();
        let mut bytes = Vec::from(header.encode());
        bytes.extend_from_slice(b"abcextra");
        let result = decode_frame_parts(&bytes);
        assert!(result.is_ok());
        let (_, payload, remainder) = result.unwrap();
        assert_eq!(payload, b"abc");
        assert_eq!(remainder, b"extra");
    }

    #[test]
    fn decode_frame_parts_truncated_payload() {
        let header = MessageHeader::new(MessageCode::Info, 10).unwrap();
        let mut bytes = Vec::from(header.encode());
        bytes.extend_from_slice(b"short"); // Only 5 bytes, header says 10
        let result = decode_frame_parts(&bytes);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn map_allocation_error_creates_out_of_memory() {
        // Create a TryReserveError by attempting to reserve too much
        let mut v: Vec<u8> = Vec::new();
        if let Err(e) = v.try_reserve(usize::MAX) {
            let io_err = map_allocation_error(e);
            assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);
        }
    }
}
