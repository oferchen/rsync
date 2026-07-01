//! Async multiplex read leaf, gated on the `tokio-transfer` feature.
//!
//! This is the `.await`-driven counterpart to the blocking
//! [`recv_msg_into`](super::recv::recv_msg_into) leaf. It exists because the
//! ASY-7 scoping result (`docs/design/asy-7-receiver-tokio-prototype.md`)
//! established that the receiver's real socket-read leaf is
//! `protocol::recv_msg_into`, and a genuine receiver-side `.await` needs an
//! async variant there rather than in `transfer` alone.
//!
//! The variant shares the header decode and payload buffer preparation with the
//! sync leaf via `super::super::helpers`, so the two can never diverge on
//! framing (tag byte, length prefix, reuse/allocation, truncation errors). The
//! only difference is how bytes are pulled off the wire: `.await` on
//! [`AsyncRead`] here versus a blocking `read` in the sync leaf.
//!
//! Additive and unwired: this leaf is not connected to `MultiplexReader` or the
//! receiver pipeline. Wiring is a later rung (the coupled ASY-7 redo).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::super::helpers::{decode_header, prepare_payload_buffer, truncated_payload_error};

/// Receives the next multiplexed message into a caller-provided buffer,
/// awaiting the underlying [`AsyncRead`] rather than blocking.
///
/// Byte-for-byte equivalent to [`recv_msg_into`](super::recv::recv_msg_into):
/// it reads the 4-byte header, decodes it through the shared `decode_header`
/// seam, clears and resizes `buffer` to the exact payload length via the shared
/// `prepare_payload_buffer` seam, and fills it. The decoded [`MessageCode`] is
/// returned so the caller can dispatch on the frame type while reading the
/// payload from `buffer`.
///
/// Errors mirror the sync leaf exactly: an invalid header surfaces as
/// [`io::ErrorKind::InvalidData`], and a payload short-read surfaces as
/// [`io::ErrorKind::UnexpectedEof`] via `truncated_payload_error`.
pub async fn recv_msg_into_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
) -> io::Result<MessageCode> {
    let header = read_header_async(reader).await?;
    let len = header.payload_len_usize();

    read_payload_into_async(reader, buffer, len).await?;

    Ok(header.code())
}

async fn read_header_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
) -> io::Result<MessageHeader> {
    let mut header_bytes = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    decode_header(&header_bytes)
}

async fn read_payload_into_async<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    len: usize,
) -> io::Result<()> {
    if !prepare_payload_buffer(buffer, len)? {
        return Ok(());
    }

    let mut read_total = 0;
    while read_total < len {
        match reader.read(&mut buffer[read_total..]).await {
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
