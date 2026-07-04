//! Async twin of the compressed receiver reader layer.
//!
//! This is the `.await`-driven counterpart to the sync `Compressed` arm of
//! [`ServerReader`](super::ServerReader), which is
//! `CompressedReader<MultiplexReader<R>>`: the sync decoder pulls
//! demultiplexed compressed wire bytes off the multiplex layer and inflates
//! them. This module supplies the analogous type for an
//! [`AsyncRead`](tokio::io::AsyncRead) transport so the later async receiver
//! fork has a concrete `R: AsyncRead` compressed reader to run over.
//!
//! # The decompression seam (why this is a drain-then-decode twin)
//!
//! The transfer crate's [`CompressedReader`] wraps the `compress` crate's
//! flate2/zstd/lz4 decoders, which are strictly `std::io::Read`-based and expose
//! no sans-io / resumable driver at this layer (unlike
//! `protocol::wire::CompressedTokenDecoder`, whose sync and async token drivers
//! share one sans-io core and differ only in the byte fetch). A streaming
//! `poll_read` twin would have to suspend *inside* a flate2 `read`, which the
//! high-level `Read` wrapper cannot do without corrupting the inflate stream
//! state (a mid-stream `Ok(0)` finalizes the deflate stream prematurely).
//!
//! What *is* byte-identical, and is what this twin does, rests on a property of
//! the layering: decompression is a **pure function of the demultiplexed
//! compressed byte sequence**. Every wire-visible, order-sensitive side effect
//! (the `MSG_INFO`/`MSG_ERROR`/... control-frame print/flush) lives in the
//! multiplex layer *below* decompression, in the shared reader-free
//! [`dispatch_message_with`](super::MultiplexReader) core, and is already
//! sync-vs-async parity-proven. So driving the **async** multiplex demux to
//! produce the identical compressed byte sequence and then inflating it with the
//! **existing sync** [`CompressedReader`] yields byte-identical decompressed
//! output and `bytes_read` to the sync path. Only the byte fetch differs
//! (`.await` on the multiplex demux versus a blocking read), which is exactly the
//! sans-io principle the rest of the async stack follows.
//!
//! The compressed segment a receiver decodes is bounded by the wire, so draining
//! it before decode does not unbound memory in the intended use. This matches how
//! `CompressedTokenDecoder::recv_token_async` handles a compressed token: read
//! the framed compressed bytes off the async transport, then inflate in memory.
//!
//! Additive and unwired: only the parity tests drive this type. It is wired by
//! the async receiver fork (ASY Stage 2C).

use std::io::{self, Cursor, Read};

use compress::algorithm::CompressionAlgorithm;

use crate::compressed_reader::CompressedReader;

/// Async twin of the sync `Compressed` reader arm, gated on `tokio-transfer`.
///
/// Decodes a compressed multiplexed stream pulled from an
/// [`AsyncRead`](tokio::io::AsyncRead) transport, producing the byte-identical
/// decompressed output the sync `CompressedReader<MultiplexReader<R>>` produces
/// for the same wire. See the module docs for the decompression-seam argument.
///
/// The compressed wire bytes are drained from the async source into an internal
/// buffer, then inflated by the shared sync [`CompressedReader`] - no decode
/// logic is duplicated. `read_async` then hands decompressed bytes out
/// incrementally, matching the chunking a sync `Read::read` on the same buffer
/// size would produce.
#[cfg(feature = "tokio-transfer")]
pub(crate) struct AsyncCompressedReader {
    /// Sync decoder over the drained compressed bytes. `None` until the first
    /// `read_async` drains the async source and constructs the decoder.
    decoder: Option<CompressedReader<Cursor<Vec<u8>>>>,
    /// Selected decompression algorithm, used to build `decoder` lazily.
    algorithm: CompressionAlgorithm,
}

#[cfg(feature = "tokio-transfer")]
impl AsyncCompressedReader {
    /// Creates a compressed async reader for `algorithm`.
    ///
    /// Mirrors [`CompressedReader::new`] in accepting the algorithm; the inner
    /// sync decoder is constructed lazily on the first `read_async` once the
    /// compressed source has been drained (so an unsupported algorithm surfaces
    /// the same error [`CompressedReader::new`] would raise, at first read).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(algorithm: CompressionAlgorithm) -> Self {
        Self {
            decoder: None,
            algorithm,
        }
    }

    /// Reads decompressed data into `buf`, awaiting the compressed source.
    ///
    /// On the first call, drains the entire bounded compressed segment from
    /// `source` (via [`AsyncReadExt::read`](tokio::io::AsyncReadExt::read)) into
    /// an internal buffer and builds the shared sync [`CompressedReader`] over it;
    /// subsequent calls inflate incrementally from that decoder. Because the
    /// decoder decodes the identical compressed byte sequence the sync path would,
    /// the delivered bytes and `bytes_read` are byte-identical to the sync
    /// `CompressedReader<MultiplexReader<R>>`.
    ///
    /// `source` should already deliver *demultiplexed* compressed bytes - in the
    /// receiver fork that is an [`AsyncServerReader`](super::AsyncServerReader) in
    /// multiplex mode, whose control-frame side-effect ordering is independently
    /// parity-proven. This layer performs no framing and no dispatch.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn read_async<R>(
        &mut self,
        source: &mut R,
        buf: &mut [u8],
    ) -> io::Result<usize>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        if self.decoder.is_none() {
            let compressed = drain_async(source).await?;
            let decoder = CompressedReader::new(Cursor::new(compressed), self.algorithm)?;
            self.decoder = Some(decoder);
        }
        // Safe: constructed just above if it was `None`.
        let decoder = self.decoder.as_mut().expect("decoder initialized");
        decoder.read(buf)
    }
}

/// Drains an [`AsyncRead`](tokio::io::AsyncRead) to EOF into a `Vec`.
///
/// The bounded compressed segment a receiver decodes fits in memory; draining it
/// once is the twin's byte-fetch step (the `.await` analogue of the sync
/// decoder's blocking pulls). Reads in 64 KiB chunks - the same
/// `MULTIPLEX_READER_BUFFER_CAPACITY` / upstream `IO_BUFFER_SIZE` unit the
/// multiplex layer frames on - so no partial-frame skew is introduced.
#[cfg(feature = "tokio-transfer")]
async fn drain_async<R>(source: &mut R) -> io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    use tokio::io::AsyncReadExt;

    let mut out = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match source.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            // A multiplexed source surfaces end-of-wire (a frame read past the
            // last frame) as UnexpectedEof rather than Ok(0); it is the end of
            // the bounded compressed segment, so drain terminates cleanly - the
            // async analogue of the sync loop's UnexpectedEof break.
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}
