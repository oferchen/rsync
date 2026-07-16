//! Sync vs async wire-byte parity for the small protocol read leaves.
//!
//! Proves the `..._async` twins of `read_varint`, `read_varlong`,
//! `read_longint`, `read_int`, the NDX codec `read_ndx`, `DeleteStats::read_from`,
//! and the protocol codec `read_stat` parse byte-identically to their blocking
//! siblings: for the same wire bytes they return the same value and consume the
//! same number of bytes, including when the bytes are delivered in tiny chunks
//! across `.await` points.
//!
//! Gated on `tokio-transfer` - compiles to nothing in the default build.
#![cfg(feature = "tokio-transfer")]

use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use protocol::codec::{create_ndx_codec, create_protocol_codec};
use protocol::{
    DeleteStats, read_int, read_int_async, read_longint, read_longint_async, read_varint,
    read_varint_async, read_varlong, read_varlong_async, write_int, write_longint, write_varint,
    write_varlong,
};

/// An [`AsyncRead`] that yields at most `chunk` bytes per `poll_read`, forcing
/// each async read leaf to reassemble its value across multiple polls / awaits.
struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl ChunkedReader {
    fn new(bytes: Vec<u8>, chunk: usize) -> Self {
        Self {
            inner: Cursor::new(bytes),
            chunk: chunk.max(1),
        }
    }

    /// Bytes consumed so far - used to assert the async leaf reads exactly as
    /// many bytes as the sync leaf.
    fn consumed(&self) -> u64 {
        self.inner.position()
    }
}

impl AsyncRead for ChunkedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let limit = self.chunk.min(buf.remaining());
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

/// Bytes consumed by the sync reader given a `Cursor`.
fn sync_consumed(cursor: &Cursor<Vec<u8>>) -> u64 {
    cursor.position()
}

/// Chunk sizes exercised for every leaf, per the ASY task spec.
const CHUNKS: [usize; 5] = [1, 2, 3, 7, 13];

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_varint() {
    let cases: [i32; 12] = [
        0,
        1,
        63,
        64,
        127,
        128,
        255,
        0x3FFF,
        0x0001_0000,
        i32::MAX,
        -1,
        i32::MIN,
    ];
    for value in cases {
        let mut wire = Vec::new();
        write_varint(&mut wire, value).unwrap();

        let mut sync_cur = Cursor::new(wire.clone());
        let sync_val = read_varint(&mut sync_cur).unwrap();
        assert_eq!(sync_val, value, "sync varint decode mismatch for {value}");

        for chunk in CHUNKS {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let async_val = read_varint_async(&mut reader).await.unwrap();
            assert_eq!(
                async_val, sync_val,
                "async varint diverged from sync for {value} at chunk {chunk}"
            );
            assert_eq!(
                reader.consumed(),
                sync_consumed(&sync_cur),
                "async varint consumed a different byte count for {value} at chunk {chunk}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_varlong() {
    let cases: [i64; 10] = [
        0,
        1,
        255,
        0x1_0000,
        0x00FF_FFFF,
        0x0001_0000_0000,
        i64::MAX,
        -1,
        -0x1_0000_0000,
        i64::MIN,
    ];
    for &min_bytes in &[3u8, 4] {
        for value in cases {
            let mut wire = Vec::new();
            write_varlong(&mut wire, value, min_bytes).unwrap();

            let mut sync_cur = Cursor::new(wire.clone());
            let sync_val = read_varlong(&mut sync_cur, min_bytes).unwrap();
            assert_eq!(sync_val, value, "sync varlong mismatch for {value}");

            for chunk in CHUNKS {
                let mut reader = ChunkedReader::new(wire.clone(), chunk);
                let async_val = read_varlong_async(&mut reader, min_bytes).await.unwrap();
                assert_eq!(
                    async_val, sync_val,
                    "async varlong diverged for {value} min_bytes {min_bytes} chunk {chunk}"
                );
                assert_eq!(
                    reader.consumed(),
                    sync_consumed(&sync_cur),
                    "async varlong byte count mismatch for {value} min_bytes {min_bytes} chunk {chunk}"
                );
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_longint() {
    let cases: [i64; 8] = [
        0,
        1,
        i32::MAX as i64,
        i32::MIN as i64,
        -2,
        0x1_0000_0000,
        i64::MAX,
        i64::MIN,
    ];
    for value in cases {
        let mut wire = Vec::new();
        write_longint(&mut wire, value).unwrap();

        let mut sync_cur = Cursor::new(wire.clone());
        let sync_val = read_longint(&mut sync_cur).unwrap();
        assert_eq!(sync_val, value, "sync longint mismatch for {value}");

        for chunk in CHUNKS {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let async_val = read_longint_async(&mut reader).await.unwrap();
            assert_eq!(
                async_val, sync_val,
                "async longint diverged for {value} chunk {chunk}"
            );
            assert_eq!(
                reader.consumed(),
                sync_consumed(&sync_cur),
                "async longint byte count mismatch for {value} chunk {chunk}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_int() {
    let cases: [i32; 6] = [0, 1, -1, i32::MAX, i32::MIN, 0x1234_5678];
    for value in cases {
        let mut wire = Vec::new();
        write_int(&mut wire, value).unwrap();

        let mut sync_cur = Cursor::new(wire.clone());
        let sync_val = read_int(&mut sync_cur).unwrap();
        assert_eq!(sync_val, value);

        for chunk in CHUNKS {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let async_val = read_int_async(&mut reader).await.unwrap();
            assert_eq!(async_val, sync_val, "async int diverged for {value}");
            assert_eq!(reader.consumed(), sync_consumed(&sync_cur));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_ndx() {
    // Drive a stream of NDX values through both a sync codec and an async codec
    // for each protocol, proving the delta state stays in lockstep and every
    // value decodes identically. Legacy (29) uses 4-byte LE; modern (32) uses
    // the delta byte-reduction form.
    let ndx_stream: [i32; 9] = [0, 1, 5, 4, 300, -1, -2, 0x10_0000, 7];

    for protocol in [29u8, 32] {
        // Encode the stream once with a writer codec.
        let mut writer_codec = create_ndx_codec(protocol);
        let mut wire = Vec::new();
        for &ndx in &ndx_stream {
            use protocol::codec::NdxCodec;
            writer_codec.write_ndx(&mut wire, ndx).unwrap();
        }

        // Sync decode of the whole stream.
        let mut sync_cur = Cursor::new(wire.clone());
        let mut sync_codec = create_ndx_codec(protocol);
        let mut sync_vals = Vec::new();
        {
            use protocol::codec::NdxCodec;
            for _ in 0..ndx_stream.len() {
                sync_vals.push(sync_codec.read_ndx(&mut sync_cur).unwrap());
            }
        }

        for chunk in CHUNKS {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let mut async_codec = create_ndx_codec(protocol);
            let mut async_vals = Vec::new();
            for _ in 0..ndx_stream.len() {
                async_vals.push(async_codec.read_ndx_async(&mut reader).await.unwrap());
            }
            assert_eq!(
                async_vals, sync_vals,
                "async read_ndx diverged from sync (protocol {protocol}, chunk {chunk})"
            );
            assert_eq!(
                reader.consumed(),
                sync_consumed(&sync_cur),
                "async read_ndx byte count mismatch (protocol {protocol}, chunk {chunk})"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_delete_stats() {
    let cases = [
        DeleteStats::new(),
        DeleteStats {
            files: 10,
            dirs: 3,
            symlinks: 2,
            devices: 1,
            specials: 4,
        },
        DeleteStats {
            // At the MAX_WIRE_DEL_STAT cap (rsync.h:181-187 = 1 << 28); a larger
            // value is now rejected as a wire-overflow guard.
            files: 1 << 28,
            dirs: 0,
            symlinks: 1,
            devices: 0x1234,
            specials: 0x0010_0000,
        },
    ];

    for stats in cases {
        let mut wire = Vec::new();
        stats.write_to(&mut wire).unwrap();

        let mut sync_cur = Cursor::new(wire.clone());
        let sync_stats = DeleteStats::read_from(&mut sync_cur).unwrap();
        assert_eq!(sync_stats, stats);

        for chunk in CHUNKS {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let async_stats = DeleteStats::read_from_async(&mut reader).await.unwrap();
            assert_eq!(
                async_stats, sync_stats,
                "async DeleteStats diverged from sync at chunk {chunk}"
            );
            assert_eq!(
                reader.consumed(),
                sync_consumed(&sync_cur),
                "async DeleteStats byte count mismatch at chunk {chunk}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_leaf_parity_stat() {
    use protocol::codec::ProtocolCodec;

    let values: [i64; 7] = [0, 1, 1000, i32::MAX as i64, 0x1_0000_0000, -1, i64::MAX];

    for protocol in [29u8, 32] {
        for value in values {
            let codec = create_protocol_codec(protocol);
            let mut wire = Vec::new();
            codec.write_stat(&mut wire, value).unwrap();

            let mut sync_cur = Cursor::new(wire.clone());
            let sync_val = codec.read_stat(&mut sync_cur).unwrap();

            for chunk in CHUNKS {
                let mut reader = ChunkedReader::new(wire.clone(), chunk);
                let async_val = codec.read_stat_async(&mut reader).await.unwrap();
                assert_eq!(
                    async_val, sync_val,
                    "async read_stat diverged (protocol {protocol}, value {value}, chunk {chunk})"
                );
                assert_eq!(
                    reader.consumed(),
                    sync_consumed(&sync_cur),
                    "async read_stat byte count mismatch (protocol {protocol}, value {value}, chunk {chunk})"
                );
            }
        }
    }
}
