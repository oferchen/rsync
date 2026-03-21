use std::io::{self, IoSlice, Write};
use std::slice;

use logging::debug_log;

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::super::frame::MessageFrame;
use super::super::helpers::{ensure_payload_length, map_envelope_error_for_input};

/// Sends a multiplexed message to `writer` using the upstream rsync envelope format.
///
/// The payload length is validated against [`crate::MAX_PAYLOAD_LENGTH`], mirroring the
/// 24-bit limit imposed by the C implementation. Violations result in
/// [`io::ErrorKind::InvalidInput`].
pub fn send_msg<W: Write>(writer: &mut W, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    debug_log!(Io, 3, "mux send: code={:?} len={}", code, payload.len());
    let payload_len = ensure_payload_length(payload.len())?;
    let header = MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;

    write_validated_message(writer, header, payload)
}

/// Sends an already constructed [`MessageFrame`] over `writer`.
///
/// The helper mirrors [`send_msg`] but allows callers that already decoded or constructed a
/// [`MessageFrame`] to transmit it without manually splitting the frame into its tag and payload.
/// The payload length is recomputed through [`MessageFrame::header`] to catch mutations performed via
/// [`::core::ops::DerefMut`], and the upstream-compatible encoding is reused through the same vectored write
/// path. [`MessageFrame::encode_into_writer`] forwards to this helper for ergonomic access from an
/// owned frame.
pub fn send_frame<W: Write>(writer: &mut W, frame: &MessageFrame) -> io::Result<()> {
    let header = frame.header()?;
    write_validated_message(writer, header, frame.payload())
}

/// Sends multiple multiplexed messages in a single vectored write operation.
///
/// This function batches multiple messages into a single `writev` syscall to reduce
/// syscall overhead when sending multiple small messages. Each message is specified
/// as a `(MessageCode, &[u8])` tuple. The payload length for each message is validated
/// against [`crate::MAX_PAYLOAD_LENGTH`].
///
/// # Performance
///
/// This function is significantly more efficient than calling [`send_msg`] repeatedly
/// when sending multiple messages, as it reduces the number of syscalls from N to 1.
///
/// # Errors
///
/// Returns an error if:
/// - Any payload exceeds [`crate::MAX_PAYLOAD_LENGTH`]
/// - The underlying write operation fails
/// - The writer reports writing more bytes than provided
///
/// # Example
///
/// ```no_run
/// use protocol::{send_msgs_vectored, MessageCode};
///
/// let mut buffer = Vec::new();
/// let messages = [
///     (MessageCode::Info, b"first message".as_slice()),
///     (MessageCode::Warning, b"second message".as_slice()),
/// ];
/// send_msgs_vectored(&mut buffer, &messages)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn send_msgs_vectored<W: Write>(
    writer: &mut W,
    messages: &[(MessageCode, &[u8])],
) -> io::Result<()> {
    if messages.is_empty() {
        return Ok(());
    }

    let mut headers = Vec::with_capacity(messages.len());
    for (code, payload) in messages {
        let payload_len = ensure_payload_length(payload.len())?;
        let header =
            MessageHeader::new(*code, payload_len).map_err(map_envelope_error_for_input)?;
        headers.push(header);
    }

    let encoded_headers: Vec<[u8; HEADER_LEN]> = headers.iter().map(|h| h.encode()).collect();

    let mut slices = Vec::with_capacity(messages.len() * 2);
    for (i, (_, payload)) in messages.iter().enumerate() {
        slices.push(IoSlice::new(&encoded_headers[i]));

        if !payload.is_empty() {
            slices.push(IoSlice::new(payload));
        }
    }

    write_all_vectored_slices(writer, &slices)
}

/// Sends a keepalive (MSG_NOOP) message to prevent connection timeouts.
///
/// Upstream rsync periodically sends `MSG_NOOP` with an empty payload as a
/// heartbeat when the sender may be silent for extended periods, such as during
/// large file checksumming. The receiver silently discards these messages.
///
/// # Examples
///
/// ```
/// use protocol::send_keepalive;
///
/// let mut buffer = Vec::new();
/// send_keepalive(&mut buffer).expect("keepalive must succeed");
/// assert_eq!(buffer.len(), 4); // header only, no payload
/// ```
pub fn send_keepalive<W: Write>(writer: &mut W) -> io::Result<()> {
    send_msg(writer, MessageCode::NoOp, &[])
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

/// Writes all IoSlices using vectored I/O with proper error handling.
///
/// Works with a slice of IoSlices rather than just two buffers, allowing
/// batching of multiple messages into fewer syscalls.
fn write_all_vectored_slices<W: Write + ?Sized>(
    writer: &mut W,
    slices: &[IoSlice<'_>],
) -> io::Result<()> {
    if slices.is_empty() {
        return Ok(());
    }

    let total_bytes: usize = slices.iter().map(|s| s.len()).sum();
    let mut written_total = 0usize;
    let mut use_vectored = true;

    while written_total < total_bytes {
        let remaining = total_bytes - written_total;

        let written = if use_vectored {
            // Calculate which slices still need to be written
            let mut accumulated = 0;
            let mut start_idx = 0;
            let mut offset_in_first = 0;

            for (i, slice) in slices.iter().enumerate() {
                if accumulated + slice.len() > written_total {
                    start_idx = i;
                    offset_in_first = written_total - accumulated;
                    break;
                }
                accumulated += slice.len();
            }

            loop {
                let mut remaining_slices = Vec::with_capacity(slices.len() - start_idx);

                for (i, slice) in slices[start_idx..].iter().enumerate() {
                    if i == 0 && offset_in_first > 0 {
                        let slice_data = &slice[offset_in_first..];
                        if !slice_data.is_empty() {
                            remaining_slices.push(IoSlice::new(slice_data));
                        }
                    } else {
                        remaining_slices.push(IoSlice::new(slice));
                    }
                }

                match writer.write_vectored(&remaining_slices) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed messages",
                        ));
                    }
                    Ok(n) => break n,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(ref err)
                        if err.kind() == io::ErrorKind::Unsupported
                            || err.kind() == io::ErrorKind::InvalidInput =>
                    {
                        use_vectored = false;
                        break 0;
                    }
                    Err(err) => return Err(err),
                }
            }
        } else {
            // Fallback to sequential writes
            let mut accumulated = 0;
            let mut current_idx = 0;
            let mut offset = 0;

            for (i, slice) in slices.iter().enumerate() {
                if accumulated + slice.len() > written_total {
                    current_idx = i;
                    offset = written_total - accumulated;
                    break;
                }
                accumulated += slice.len();
            }

            loop {
                let current_slice = &slices[current_idx];
                let slice_data = &current_slice[offset..];

                match writer.write(slice_data) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed messages",
                        ));
                    }
                    Ok(n) => break n,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
        };

        if !use_vectored && written == 0 {
            continue;
        }

        if written > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "writer reported writing {written} bytes but only {remaining} bytes remained"
                ),
            ));
        }

        written_total += written;
    }

    Ok(())
}

/// Vectored write of a header-payload pair with fallback to sequential writes.
///
/// Exposed as `pub(super)` for testing from sibling modules.
pub(super) fn write_all_vectored<W: Write + ?Sized>(
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
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
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
