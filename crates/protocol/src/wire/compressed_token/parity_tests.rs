//! Sync vs async parity tests for the sans-io compressed-token decoder.
//!
//! These prove that [`CompressedTokenDecoder::recv_token_async`] decodes
//! byte-for-byte identically to the blocking
//! [`CompressedTokenDecoder::recv_token`], across all three algorithms and
//! every decode path the sans-io state machine exercises: multi-block deflated
//! literals (the accumulation loop), dictionary carryover
//! (CPRES_ZLIB `see_token`), zero-output continues, token runs, and absolute
//! `TOKEN_LONG` tokens. Both whole-stream delivery and byte-at-a-time chunked
//! delivery are checked so the async byte-fetch is driven across many
//! `.await` points.
//!
//! Because both drivers advance the exact same [`TokenStepper`](super::step)
//! state machine, any divergence here would be a driver bug, not a decode bug.
//! This is the token half of the `async-wire-parity` CI gate.

use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use compress::zlib::CompressionLevel;
use tokio::io::{AsyncRead, ReadBuf};

use super::{CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder};

/// How to construct a fresh matching encoder/decoder pair for an algorithm.
#[derive(Clone, Copy)]
enum Algo {
    Zlib,
    Zlibx,
    #[cfg(feature = "zstd")]
    Zstd,
    #[cfg(feature = "lz4")]
    Lz4,
}

impl Algo {
    fn encoder(self) -> CompressedTokenEncoder {
        match self {
            Algo::Zlib => CompressedTokenEncoder::new(CompressionLevel::Default, 31),
            Algo::Zlibx => {
                let mut enc = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
                enc.set_zlibx(true);
                enc
            }
            #[cfg(feature = "zstd")]
            Algo::Zstd => CompressedTokenEncoder::new_zstd(3, None).unwrap(),
            #[cfg(feature = "lz4")]
            Algo::Lz4 => CompressedTokenEncoder::new_lz4(),
        }
    }

    fn decoder(self) -> CompressedTokenDecoder {
        match self {
            Algo::Zlib => CompressedTokenDecoder::new(),
            Algo::Zlibx => {
                let mut dec = CompressedTokenDecoder::new();
                dec.set_zlibx(true);
                dec
            }
            #[cfg(feature = "zstd")]
            Algo::Zstd => CompressedTokenDecoder::new_zstd().unwrap(),
            #[cfg(feature = "lz4")]
            Algo::Lz4 => CompressedTokenDecoder::new_lz4(),
        }
    }

    fn all() -> Vec<Algo> {
        vec![
            Algo::Zlib,
            Algo::Zlibx,
            #[cfg(feature = "zstd")]
            Algo::Zstd,
            #[cfg(feature = "lz4")]
            Algo::Lz4,
        ]
    }
}

/// A scripted transfer operation used to build a representative wire corpus.
enum Op {
    Literal(Vec<u8>),
    Block(u32),
}

/// Builds a corpus that exercises multi-block deflated literals, dictionary
/// carryover, token runs, absolute tokens, and zero-output continues.
fn corpus() -> Vec<Op> {
    // A large incompressible literal forces multiple DEFLATED_DATA blocks and,
    // for CPRES_ZLIB, output that spans several CHUNK_SIZE emissions.
    let mut big = Vec::with_capacity(200_000);
    for i in 0..200_000usize {
        big.push((i.wrapping_mul(2654435761) >> 13) as u8);
    }
    // A highly compressible literal.
    let repetitive = vec![0x5Au8; 100_000];

    vec![
        Op::Literal(b"first literal".to_vec()),
        Op::Block(0),
        Op::Block(1),
        Op::Block(2),
        Op::Literal(repetitive),
        // Absolute token far from the last, forcing TOKEN_LONG encoding.
        Op::Block(100_000),
        Op::Block(100_001),
        Op::Literal(big),
        Op::Block(5),
        Op::Literal(b"tail".to_vec()),
    ]
}

/// Encodes `ops` to a single wire buffer with the given algorithm. Feeds block
/// data to the encoder's dictionary (`see_token`) exactly as a real sender
/// would, so CPRES_ZLIB dictionary carryover is exercised on the wire.
fn encode(algo: Algo, ops: &[Op]) -> Vec<u8> {
    let mut enc = algo.encoder();
    let mut wire = Vec::new();
    for op in ops {
        match op {
            Op::Literal(data) => enc.send_literal(&mut wire, data).unwrap(),
            Op::Block(idx) => {
                enc.send_block_match(&mut wire, *idx).unwrap();
                // Feed synthetic basis-block bytes into the dictionary, matching
                // the receiver's see_token below.
                enc.see_token(&block_bytes(*idx)).unwrap();
            }
        }
    }
    enc.finish(&mut wire).unwrap();
    wire
}

/// Deterministic synthetic basis-block bytes for a given block index.
fn block_bytes(idx: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    for i in 0..64u32 {
        v.push((idx.wrapping_add(i).wrapping_mul(31)) as u8);
    }
    v
}

/// Drains all tokens from the blocking decoder, feeding block data into the
/// decoder dictionary via `see_token` on each block match.
fn decode_sync(algo: Algo, wire: &[u8]) -> Vec<CompressedToken> {
    let mut dec = algo.decoder();
    let mut reader = Cursor::new(wire);
    let mut out = Vec::new();
    loop {
        let tok = dec.recv_token(&mut reader).unwrap();
        if let CompressedToken::BlockMatch(idx) = tok {
            dec.see_token(&block_bytes(idx)).unwrap();
        }
        let end = matches!(tok, CompressedToken::End);
        out.push(tok);
        if end {
            break;
        }
    }
    out
}

/// Drains all tokens from the async decoder over `reader`.
async fn decode_async<R: AsyncRead + Unpin>(algo: Algo, reader: &mut R) -> Vec<CompressedToken> {
    let mut dec = algo.decoder();
    let mut out = Vec::new();
    loop {
        let tok = dec.recv_token_async(reader).await.unwrap();
        if let CompressedToken::BlockMatch(idx) = tok {
            dec.see_token(&block_bytes(idx)).unwrap();
        }
        let end = matches!(tok, CompressedToken::End);
        out.push(tok);
        if end {
            break;
        }
    }
    out
}

/// An [`AsyncRead`] that yields at most `chunk` bytes per poll, forcing the
/// async driver to reassemble every read across many `.await` points.
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
async fn token_parity_whole_stream() {
    let ops = corpus();
    for algo in Algo::all() {
        let wire = encode(algo, &ops);
        let sync_tokens = decode_sync(algo, &wire);

        let mut reader = Cursor::new(wire.clone());
        let async_tokens = decode_async(algo, &mut reader).await;

        assert_eq!(
            async_tokens, sync_tokens,
            "async token decoder diverged from sync on whole-stream delivery"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn token_parity_chunked_delivery() {
    let ops = corpus();
    for algo in Algo::all() {
        let wire = encode(algo, &ops);
        let sync_tokens = decode_sync(algo, &wire);

        for chunk in [1usize, 2, 3, 7, 13] {
            let mut reader = ChunkedReader {
                inner: Cursor::new(wire.clone()),
                chunk,
            };
            let async_tokens = decode_async(algo, &mut reader).await;
            assert_eq!(
                async_tokens, sync_tokens,
                "async token decoder diverged from sync with chunk size {chunk}"
            );
        }
    }
}
