//! In-process channel adapters that bridge `tokio::sync::mpsc` to
//! `AsyncRead`/`AsyncWrite`.
//!
//! These adapters let an async transport (for example, the embedded SSH
//! transport) expose its stdio over an in-process duplex channel while still
//! satisfying APIs that require raw `tokio::io::AsyncRead` and
//! `tokio::io::AsyncWrite` implementations.
//!
//! # Overview
//!
//! - `ChannelReader` consumes byte chunks from an `mpsc::Receiver<Vec<u8>>`
//!   and presents them through `AsyncRead`. Oversized chunks that do not fit
//!   in the caller-provided `ReadBuf` are buffered internally and drained on
//!   subsequent reads.
//! - `ChannelWriter` forwards each `AsyncWrite::poll_write` call to an
//!   `mpsc::Sender<Vec<u8>>`. When the channel is full, the writer registers
//!   the task waker and returns `Poll::Pending`; the writer wakes when
//!   capacity becomes available.
//! - `pair` returns two cross-connected `(ChannelReader, ChannelWriter)`
//!   halves so a single duplex channel can be split between two peers.
//!
//! # Invariants
//!
//! - A closed sender on the read side yields `Ok(0)` to signal EOF, matching
//!   standard tokio stream semantics.
//! - A closed receiver on the write side surfaces as
//!   [`std::io::ErrorKind::BrokenPipe`] from `poll_write`.
//! - Each `poll_write` consumes the entire input slice in a single channel
//!   message, mirroring how SSH stdio chunks are framed in upstream rsync's
//!   embedded transport.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

/// Asynchronous reader that drains byte chunks delivered through an
/// `mpsc::Receiver<Vec<u8>>`.
///
/// Each received chunk is copied into the caller's `ReadBuf`. When a chunk
/// exceeds the buffer's remaining capacity, the unread tail is retained and
/// served on subsequent calls before another chunk is pulled from the channel.
pub struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buffered: Vec<u8>,
    offset: usize,
}

impl ChannelReader {
    /// Wraps the supplied receiver in an `AsyncRead` adapter.
    pub fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffered: Vec::new(),
            offset: 0,
        }
    }

    fn drain_buffered(&mut self, buf: &mut ReadBuf<'_>) -> usize {
        let remaining = &self.buffered[self.offset..];
        let n = remaining.len().min(buf.remaining());
        if n == 0 {
            return 0;
        }
        buf.put_slice(&remaining[..n]);
        self.offset += n;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        n
    }
}

impl AsyncRead for ChannelReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.buffered.is_empty() {
            self.drain_buffered(buf);
            return Poll::Ready(Ok(()));
        }
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                if chunk.is_empty() {
                    // Skip zero-length frames and wait for more data.
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                self.buffered = chunk;
                self.offset = 0;
                self.drain_buffered(buf);
                Poll::Ready(Ok(()))
            }
            // Closed channel signals EOF via an immediate Ready with no bytes
            // written, matching tokio's AsyncRead contract.
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Boxed reservation future used by `ChannelWriter` to preserve the
/// channel's wait-queue registration across `poll_write` invocations.
type ReservationFuture = Pin<
    Box<dyn Future<Output = Result<mpsc::OwnedPermit<Vec<u8>>, mpsc::error::SendError<()>>> + Send>,
>;

/// Asynchronous writer that forwards each write as a single message on an
/// `mpsc::Sender<Vec<u8>>`.
///
/// Backpressure is honored: when the channel is full, `poll_write` parks the
/// current task and returns `Poll::Pending` until capacity is available. A
/// closed channel surfaces as [`io::ErrorKind::BrokenPipe`]. `poll_shutdown`
/// drops the internal sender, signalling EOF to the reader half.
pub struct ChannelWriter {
    tx: Option<mpsc::Sender<Vec<u8>>>,
    // Pending reservation future preserved across polls so the channel's
    // wait-queue registration is not dropped between `poll_write` calls.
    reserving: Option<ReservationFuture>,
}

impl ChannelWriter {
    /// Wraps the supplied sender in an `AsyncWrite` adapter.
    pub fn new(tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tx: Some(tx),
            reserving: None,
        }
    }
}

impl AsyncWrite for ChannelWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        // Resume a pending reservation, if any.
        if let Some(mut reserve) = self.reserving.take() {
            match reserve.as_mut().poll(cx) {
                Poll::Ready(Ok(permit)) => {
                    permit.send(buf.to_vec());
                    return Poll::Ready(Ok(buf.len()));
                }
                Poll::Ready(Err(_)) => {
                    self.tx = None;
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "channel closed",
                    )));
                }
                Poll::Pending => {
                    self.reserving = Some(reserve);
                    return Poll::Pending;
                }
            }
        }

        let tx = match self.tx.as_ref() {
            Some(tx) => tx.clone(),
            None => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "writer shut down",
                )));
            }
        };

        match tx.try_send(buf.to_vec()) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let mut reserve = Box::pin(tx.reserve_owned());
                match reserve.as_mut().poll(cx) {
                    Poll::Ready(Ok(permit)) => {
                        permit.send(buf.to_vec());
                        Poll::Ready(Ok(buf.len()))
                    }
                    Poll::Ready(Err(_)) => {
                        self.tx = None;
                        Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "channel closed",
                        )))
                    }
                    Poll::Pending => {
                        self.reserving = Some(reserve);
                        Poll::Pending
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.tx = None;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel closed",
                )))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.reserving = None;
        self.tx = None;
        Poll::Ready(Ok(()))
    }
}

/// Builds two cross-connected `(ChannelReader, ChannelWriter)` halves over a
/// duplex `mpsc` channel pair of the requested capacity.
///
/// The first half reads what the second writes, and vice versa, providing a
/// fully in-memory duplex stream that can stand in for any `AsyncRead` +
/// `AsyncWrite` transport.
#[must_use]
pub fn pair(
    capacity: usize,
) -> (
    (ChannelReader, ChannelWriter),
    (ChannelReader, ChannelWriter),
) {
    let cap = capacity.max(1);
    let (a_tx, a_rx) = mpsc::channel::<Vec<u8>>(cap);
    let (b_tx, b_rx) = mpsc::channel::<Vec<u8>>(cap);
    (
        (ChannelReader::new(b_rx), ChannelWriter::new(a_tx)),
        (ChannelReader::new(a_rx), ChannelWriter::new(b_tx)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn round_trip_bytes() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let mut writer = ChannelWriter::new(tx);
        let mut reader = ChannelReader::new(rx);

        let payload = b"hello channel adapter".to_vec();
        writer.write_all(&payload).await.unwrap();
        writer.shutdown().await.unwrap();

        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn oversized_chunk_is_drained_across_reads() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(2);
        let mut writer = ChannelWriter::new(tx);
        let mut reader = ChannelReader::new(rx);

        writer.write_all(b"abcdefgh").await.unwrap();
        writer.shutdown().await.unwrap();

        let mut chunk = [0u8; 3];
        let n = reader.read(&mut chunk).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(&chunk[..n], b"abc");

        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"defgh");
    }

    #[tokio::test]
    async fn backpressure_blocks_until_reader_drains() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        let mut writer = ChannelWriter::new(tx);
        let mut reader = ChannelReader::new(rx);

        writer.write_all(b"first").await.unwrap();

        let write_task = tokio::spawn(async move {
            writer.write_all(b"second").await.unwrap();
            writer.shutdown().await.unwrap();
        });

        // The second write cannot complete until the reader pulls the first
        // message out of the bounded channel.
        let mut first = vec![0u8; 5];
        reader.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"first");

        let mut second = Vec::new();
        reader.read_to_end(&mut second).await.unwrap();
        assert_eq!(second, b"second");
        write_task.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_writer_yields_eof() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
        let writer = ChannelWriter::new(tx);
        let mut reader = ChannelReader::new(rx);

        drop(writer);

        let mut buf = [0u8; 8];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn shutdown_yields_eof() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
        let mut writer = ChannelWriter::new(tx);
        let mut reader = ChannelReader::new(rx);

        writer.write_all(b"bye").await.unwrap();
        writer.shutdown().await.unwrap();

        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"bye");
    }

    #[tokio::test]
    async fn write_after_shutdown_is_broken_pipe() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
        let mut writer = ChannelWriter::new(tx);
        let _reader = ChannelReader::new(rx);

        writer.shutdown().await.unwrap();
        let err = writer.write_all(b"nope").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn write_to_closed_reader_is_broken_pipe() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        let mut writer = ChannelWriter::new(tx);

        drop(rx);

        let err = writer.write_all(b"orphan").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn duplex_pair_exchanges_data_both_ways() {
        let ((mut a_read, mut a_write), (mut b_read, mut b_write)) = pair(4);

        let writer_a = tokio::spawn(async move {
            a_write.write_all(b"a->b").await.unwrap();
            a_write.shutdown().await.unwrap();
        });
        let writer_b = tokio::spawn(async move {
            b_write.write_all(b"b->a").await.unwrap();
            b_write.shutdown().await.unwrap();
        });

        let mut from_a = Vec::new();
        b_read.read_to_end(&mut from_a).await.unwrap();
        assert_eq!(from_a, b"a->b");

        let mut from_b = Vec::new();
        a_read.read_to_end(&mut from_b).await.unwrap();
        assert_eq!(from_b, b"b->a");

        writer_a.await.unwrap();
        writer_b.await.unwrap();
    }
}
