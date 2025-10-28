use std::collections::TryReserveError;
use std::io::{self, Read};

use crate::envelope::{EnvelopeError, HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageHeader};

pub(super) fn map_envelope_error(err: EnvelopeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

pub(super) fn map_envelope_error_for_input(err: EnvelopeError) -> io::Error {
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

pub(super) fn truncated_frame_error(expected: usize, actual: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("multiplexed frame truncated: expected {expected} bytes but received {actual}"),
    )
}

pub(super) fn truncated_payload_error(expected: usize, actual: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("multiplexed payload truncated: expected {expected} bytes but received {actual}"),
    )
}

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
