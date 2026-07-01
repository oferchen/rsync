//! Sync vs async wire-parity tests for the multiplex read leaf.
//!
//! These prove that [`recv_msg_into_async`](super::recv_msg_into_async) parses
//! byte-identically to the blocking [`recv_msg_into`](super::recv_msg_into)
//! leaf. Identical wire bytes are fed to both readers and every parsed frame
//! (code, length, payload bytes) is compared frame-for-frame. A chunked variant
//! drives the async reader over a byte-at-a-time delivery boundary to prove it
//! reassembles frames identically across `.await` points.
//!
//! This is the test the `async-wire-parity` CI gate runs; it replaces the
//! previous no-op that referenced a non-existent `transfer` test.

use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use crate::MAX_PAYLOAD_LENGTH;
use crate::envelope::MessageCode;

use super::{recv_msg_into, recv_msg_into_async, send_msg};

/// A parsed frame: the decoded code and the exact payload bytes.
type ParsedFrame = (MessageCode, Vec<u8>);

/// Builds the base corpus of representative multiplex frames: every message
/// code plus empty, small, single-byte, and binary payloads. Kept small enough
/// to drive byte-at-a-time through the chunked-delivery test.
fn base_frames() -> Vec<ParsedFrame> {
    let mut frames: Vec<ParsedFrame> = vec![
        // MSG_DATA with content.
        (MessageCode::Data, b"delta token payload".to_vec()),
        // MSG_INFO empty payload.
        (MessageCode::Info, Vec::new()),
        // MSG_ERROR text.
        (MessageCode::Error, b"error message text".to_vec()),
        // MSG_DATA empty (keep-alive shaped frame).
        (MessageCode::Data, Vec::new()),
        // Warning + Log control frames.
        (MessageCode::Warning, b"warning".to_vec()),
        (MessageCode::Log, b"log line".to_vec()),
        // Binary payload including NUL and high bytes.
        (MessageCode::Data, vec![0u8, 1, 2, 0xFE, 0xFF, 0x7F]),
        // Single-byte payload.
        (MessageCode::Info, vec![0x42]),
    ];

    // Every message code with a fixed payload, so all tag bytes are exercised.
    for code in MessageCode::ALL {
        frames.push((code, b"code-sweep".to_vec()));
    }

    frames
}

/// Encodes `frames` into a single wire stream via [`send_msg`].
fn encode(frames: &[ParsedFrame]) -> Vec<u8> {
    let mut wire = Vec::new();
    for (code, payload) in frames {
        send_msg(&mut wire, *code, payload).unwrap();
    }
    wire
}

/// Drains all frames from the blocking reader over `wire`.
fn parse_sync(wire: &[u8]) -> Vec<ParsedFrame> {
    let mut reader = Cursor::new(wire);
    let mut buffer = Vec::new();
    let mut out = Vec::new();
    for _ in 0.. {
        if reader.position() as usize >= wire.len() {
            break;
        }
        let code = recv_msg_into(&mut reader, &mut buffer).unwrap();
        out.push((code, buffer.clone()));
    }
    out
}

/// Drains `count` frames from an async reader.
async fn parse_async<R: AsyncRead + Unpin>(reader: &mut R, count: usize) -> Vec<ParsedFrame> {
    let mut buffer = Vec::new();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let code = recv_msg_into_async(reader, &mut buffer).await.unwrap();
        out.push((code, buffer.clone()));
    }
    out
}

/// An [`AsyncRead`] adapter that yields at most `chunk` bytes per `poll_read`,
/// forcing the async payload/header loops to reassemble frames across multiple
/// polls (and thus across `.await` points).
struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl AsyncRead for ChunkedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let chunk = self.chunk.max(1);
        let limit = chunk.min(buf.remaining());
        if limit == 0 {
            return Poll::Ready(Ok(()));
        }

        // Read at most `limit` bytes into a scratch buffer, then copy the read
        // prefix into the caller buffer. This forces frame reassembly across
        // multiple polls without any unsafe buffer manipulation.
        let mut scratch = vec![0u8; limit];
        let mut scratch_buf = ReadBuf::new(&mut scratch);
        match Pin::new(&mut self.inner).poll_read(cx, &mut scratch_buf) {
            Poll::Ready(Ok(())) => {
                buf.put_slice(scratch_buf.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn recv_msg_parity_whole_stream() {
    let mut expected = base_frames();
    // Maximum-length payload to exercise the 24-bit length prefix boundary.
    expected.push((MessageCode::Data, vec![0xABu8; MAX_PAYLOAD_LENGTH as usize]));
    let wire = encode(&expected);

    let sync_frames = parse_sync(&wire);
    assert_eq!(sync_frames.len(), expected.len());
    assert_eq!(sync_frames, expected);

    let mut reader = Cursor::new(wire.clone());
    let async_frames = parse_async(&mut reader, expected.len()).await;

    assert_eq!(
        async_frames, sync_frames,
        "async multiplex read leaf diverged from the sync leaf on whole-stream delivery"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn recv_msg_parity_chunked_delivery() {
    let expected = base_frames();
    let wire = encode(&expected);
    let sync_frames = parse_sync(&wire);

    // Deliver bytes in tiny chunks so the async header and payload loops must
    // reassemble every frame across many polls / await points.
    for chunk in [1usize, 2, 3, 7] {
        let mut reader = ChunkedReader {
            inner: Cursor::new(wire.clone()),
            chunk,
        };
        let async_frames = parse_async(&mut reader, expected.len()).await;
        assert_eq!(
            async_frames, sync_frames,
            "async multiplex read leaf diverged from the sync leaf with chunk size {chunk}"
        );
    }
}
