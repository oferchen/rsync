use std::io::{self, IoSlice, Read, Write};
use std::slice;

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::frame::MessageFrame;
use super::helpers::{
    ensure_payload_length, map_envelope_error, map_envelope_error_for_input, read_payload,
    read_payload_into,
};

/// Sends a multiplexed message to `writer` using the upstream rsync envelope format.
///
/// The payload length is validated against [`crate::MAX_PAYLOAD_LENGTH`], mirroring the
/// 24-bit limit imposed by the C implementation. Violations result in
/// [`io::ErrorKind::InvalidInput`].
pub fn send_msg<W: Write>(writer: &mut W, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    let payload_len = ensure_payload_length(payload.len())?;
    let header = MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;
    write_validated_message(writer, header, payload)
}

/// Sends an already constructed [`MessageFrame`] over `writer`.
///
/// The helper mirrors [`crate::send_msg`] but allows callers that already decoded or constructed a
/// [`MessageFrame`] to transmit it without manually splitting the frame into its tag and payload.
/// The payload length is recomputed through [`MessageFrame::header`] to catch mutations performed via
/// [`::core::ops::DerefMut`], and the upstream-compatible encoding is reused through the same vectored write
/// path. [`MessageFrame::encode_into_writer`] forwards to this helper for ergonomic access from an
/// owned frame.
pub fn send_frame<W: Write>(writer: &mut W, frame: &MessageFrame) -> io::Result<()> {
    let header = frame.header()?;
    write_validated_message(writer, header, frame.payload())
}

fn write_validated_message<W: Write + ?Sized>(
    writer: &mut W,
    header: MessageHeader,
    payload: &[u8],
) -> io::Result<()> {
    let header_bytes = header.encode();

    if payload.is_empty() {
        writer.write_all(&header_bytes)?;
        return Ok(());
    }

    write_all_vectored(writer, header_bytes.as_slice(), payload)
}

/// Receives the next multiplexed message from `reader`.
///
/// The function blocks until the full header and payload are read or an I/O
/// error occurs. Invalid headers surface as [`io::ErrorKind::InvalidData`].
pub fn recv_msg<R: Read>(reader: &mut R) -> io::Result<MessageFrame> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();

    let payload = read_payload(reader, len)?;

    MessageFrame::new(header.code(), payload)
}

/// Receives the next multiplexed message into a caller-provided buffer.
///
/// The helper mirrors [`crate::recv_msg`] but avoids allocating a new vector for every
/// frame. The buffer is cleared and then resized to the exact payload length,
/// reusing any existing capacity to satisfy the workspace's buffer reuse
/// guidance. The decoded message code is returned so the caller can dispatch on
/// the frame type while reading the payload from `buffer`.
pub fn recv_msg_into<R: Read>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<MessageCode> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();

    read_payload_into(reader, buffer, len)?;

    Ok(header.code())
}

fn read_header<R: Read>(reader: &mut R) -> io::Result<MessageHeader> {
    let mut header_bytes = [0u8; HEADER_LEN];
    let _ = std::fs::write("/tmp/mux_HEADER_BEFORE_READ", "1");
    match reader.read_exact(&mut header_bytes) {
        Ok(()) => {
            let _ = std::fs::write("/tmp/mux_HEADER_READ_OK", format!("{header_bytes:02x?}"));
            MessageHeader::decode(&header_bytes).map_err(map_envelope_error)
        }
        Err(e) => {
            let _ = std::fs::write("/tmp/mux_HEADER_READ_ERR", format!("{:?}: {}", e.kind(), e));
            Err(e)
        }
    }
}

fn write_all_vectored<W: Write + ?Sized>(
    writer: &mut W,
    mut header: &[u8],
    mut payload: &[u8],
) -> io::Result<()> {
    let mut use_vectored = true;

    'outer: while !header.is_empty() || !payload.is_empty() {
        let header_len = header.len();
        let payload_len = payload.len();
        let available = if use_vectored {
            header_len + payload_len
        } else if header_len != 0 {
            header_len
        } else {
            payload_len
        };

        let written = if use_vectored {
            loop {
                let result = if header.is_empty() {
                    let slice = IoSlice::new(payload);
                    writer.write_vectored(slice::from_ref(&slice))
                } else if payload.is_empty() {
                    let slice = IoSlice::new(header);
                    writer.write_vectored(slice::from_ref(&slice))
                } else {
                    let slices = [IoSlice::new(header), IoSlice::new(payload)];
                    writer.write_vectored(&slices)
                };

                match result {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed message",
                        ));
                    }
                    Ok(written) => break written,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(ref err)
                        if err.kind() == io::ErrorKind::Unsupported
                            || err.kind() == io::ErrorKind::InvalidInput =>
                    {
                        use_vectored = false;
                        continue 'outer;
                    }
                    Err(err) => return Err(err),
                }
            }
        } else {
            loop {
                let buffer = if !header.is_empty() { header } else { payload };
                match writer.write(buffer) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed message",
                        ));
                    }
                    Ok(written) => break written,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(err),
                }
            }
        };

        if written > available {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "writer reported writing {written} bytes but only {available} bytes were provided for multiplexed frame"
                ),
            ));
        }

        let mut remaining = written;
        if header_len != 0 {
            if remaining >= header_len {
                remaining -= header_len;
                header = &[];
            } else {
                header = &header[remaining..];
                continue;
            }
        }

        if remaining > 0 && payload_len != 0 {
            if remaining == payload_len {
                payload = &[];
            } else {
                payload = &payload[remaining..];
            }
        }
    }

    Ok(())
}
