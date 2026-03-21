use std::io::{self, Read};

use logging::debug_log;

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::super::frame::MessageFrame;
use super::super::helpers::{map_envelope_error, read_payload, read_payload_into};

/// Receives the next multiplexed message from `reader`.
///
/// The function blocks until the full header and payload are read or an I/O
/// error occurs. Invalid headers surface as [`io::ErrorKind::InvalidData`].
pub fn recv_msg<R: Read>(reader: &mut R) -> io::Result<MessageFrame> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();
    debug_log!(Io, 3, "mux recv: code={:?} len={}", header.code(), len);

    let payload = read_payload(reader, len)?;

    MessageFrame::new(header.code(), payload)
}

/// Receives the next multiplexed message into a caller-provided buffer.
///
/// The helper mirrors [`recv_msg`] but avoids allocating a new vector for every
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
    reader.read_exact(&mut header_bytes)?;
    MessageHeader::decode(&header_bytes).map_err(map_envelope_error)
}
