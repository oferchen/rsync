//! crates/protocol/src/negotiation/sniffer/async_read.rs
//!
//! Async I/O extensions for [`NegotiationPrologueSniffer`].
//!
//! This module provides async read functionality using tokio's [`AsyncRead`] trait,
//! enabling non-blocking negotiation detection in async contexts.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;

use super::super::NegotiationPrologue;
use super::{NegotiationPrologueSniffer, map_reserve_error_for_io};

impl NegotiationPrologueSniffer {
    /// Asynchronously reads from `reader` until the negotiation style can be determined.
    ///
    /// This is the async equivalent of [`Self::read_from`], using tokio's [`AsyncRead`]
    /// trait instead of `std::io::Read`. It enables non-blocking negotiation detection
    /// in async contexts.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The connection closes before the negotiation prologue is determined (UnexpectedEof)
    /// - An I/O error occurs during reading
    /// - Memory allocation for the buffer fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// use protocol::NegotiationPrologueSniffer;
    /// use tokio::net::TcpStream;
    ///
    /// async fn detect_protocol(mut stream: TcpStream) -> std::io::Result<()> {
    ///     let mut sniffer = NegotiationPrologueSniffer::new();
    ///     let prologue = sniffer.read_from_async(&mut stream).await?;
    ///
    ///     match prologue {
    ///         protocol::NegotiationPrologue::LegacyAscii => {
    ///             println!("Legacy ASCII protocol detected");
    ///         }
    ///         protocol::NegotiationPrologue::Binary => {
    ///             println!("Binary protocol detected");
    ///         }
    ///         _ => {}
    ///     }
    ///     Ok(())
    /// }
    /// ```
    pub async fn read_from_async<R>(&mut self, reader: &mut R) -> io::Result<NegotiationPrologue>
    where
        R: AsyncRead + Unpin,
    {
        // Check if we already have a decision
        match self.detector.decision() {
            Some(decision) if !self.needs_more_legacy_prefix_bytes(decision) => {
                return Ok(decision);
            }
            _ => {}
        }

        let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];

        loop {
            let cached = self.detector.decision();
            let needs_more_prefix_bytes =
                cached.is_some_and(|decision| self.needs_more_legacy_prefix_bytes(decision));
            if let Some(decision) = cached.filter(|_| !needs_more_prefix_bytes) {
                return Ok(decision);
            }

            let bytes_to_read = if needs_more_prefix_bytes {
                LEGACY_DAEMON_PREFIX_LEN - self.detector.buffered_len()
            } else {
                1
            };

            match reader.read(&mut scratch[..bytes_to_read]).await {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed before rsync negotiation prologue was determined",
                    ));
                }
                Ok(read) => {
                    let observed = &scratch[..read];
                    let (decision, consumed) =
                        self.observe(observed).map_err(map_reserve_error_for_io)?;
                    debug_assert!(consumed <= observed.len());

                    if consumed < observed.len() {
                        let remainder = &observed[consumed..];
                        if !remainder.is_empty() {
                            self.buffered
                                .try_reserve_exact(remainder.len())
                                .map_err(map_reserve_error_for_io)?;
                            self.buffered.extend_from_slice(remainder);
                        }
                    }
                    let needs_more_prefix_bytes = self.needs_more_legacy_prefix_bytes(decision);
                    if decision != NegotiationPrologue::NeedMoreData && !needs_more_prefix_bytes {
                        return Ok(decision);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::AsyncRead;

    /// Wrapper to make Cursor implement tokio's AsyncRead via read_buf
    struct AsyncCursor(Cursor<Vec<u8>>);

    impl AsyncRead for AsyncCursor {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            let slice = buf.initialize_unfilled();
            match std::io::Read::read(&mut self.0, slice) {
                Ok(n) => {
                    buf.advance(n);
                    std::task::Poll::Ready(Ok(()))
                }
                Err(e) => std::task::Poll::Ready(Err(e)),
            }
        }
    }

    #[tokio::test]
    async fn async_read_detects_legacy_ascii() {
        let data = b"@RSYNCD: 31.0\n".to_vec();
        let mut reader = AsyncCursor(Cursor::new(data));
        let mut sniffer = NegotiationPrologueSniffer::new();

        let result = sniffer.read_from_async(&mut reader).await.unwrap();
        assert_eq!(result, NegotiationPrologue::LegacyAscii);
    }

    #[tokio::test]
    async fn async_read_detects_binary() {
        // Binary protocol starts with non-@ byte
        let data = vec![0x00, 0x1f, 0x00, 0x00];
        let mut reader = AsyncCursor(Cursor::new(data));
        let mut sniffer = NegotiationPrologueSniffer::new();

        let result = sniffer.read_from_async(&mut reader).await.unwrap();
        assert_eq!(result, NegotiationPrologue::Binary);
    }

    #[tokio::test]
    async fn async_read_eof_before_decision() {
        let data = Vec::new();
        let mut reader = AsyncCursor(Cursor::new(data));
        let mut sniffer = NegotiationPrologueSniffer::new();

        let result = sniffer.read_from_async(&mut reader).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn async_read_buffers_remainder() {
        let data = b"@RSYNCD: 31.0\nextra data".to_vec();
        let mut reader = AsyncCursor(Cursor::new(data));
        let mut sniffer = NegotiationPrologueSniffer::new();

        let result = sniffer.read_from_async(&mut reader).await.unwrap();
        assert_eq!(result, NegotiationPrologue::LegacyAscii);
        // Sniffer should have buffered the prefix
        assert!(!sniffer.buffered().is_empty());
    }
}
