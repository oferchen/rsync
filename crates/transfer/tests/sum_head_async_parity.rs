//! Sync vs async wire-byte parity for `SumHead::read` / `SumHead::read_async`.
//!
//! The sum_head is the 16-byte (four 32-bit LE fields) signature header the
//! receiver reads before the block sums. This proves the `.await`-driven
//! `read_async` decodes byte-identically to the blocking `read`, including when
//! the 16 bytes are delivered one at a time across await points.
//!
//! Gated on `tokio-transfer` - compiles to nothing in the default build.
#![cfg(feature = "tokio-transfer")]

use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use transfer::SumHead;

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

#[tokio::test(flavor = "current_thread")]
async fn sum_head_read_async_matches_sync() {
    // All fixtures are valid sum_heads: `SumHead::read` now validates the wire
    // fields (upstream io.c:2025-2067) and rejects out-of-range values, so a
    // parity fixture must stay within the accepted ranges while still setting
    // high bytes across all four fields to exercise byte-level decode parity.
    let cases = [
        SumHead::empty(),
        SumHead::new(1, 700, 16, 0),
        SumHead::new(1234, 65536, 2, 511),
        // Large-but-valid: count with three nonzero bytes, blength at the legacy
        // MAX_BLOCK_SIZE ceiling (1<<29), s2length at the SHA1 cap (20),
        // remainder just below blength.
        SumHead::new(0x00FF_FFFF, 1 << 29, 20, (1 << 29) - 1),
        SumHead::new(2, 1024, 8, 1023),
    ];

    for head in cases {
        let mut wire = Vec::new();
        head.write(&mut wire).unwrap();
        assert_eq!(wire.len(), 16, "sum_head wire form must be 16 bytes");

        let mut sync_cur = Cursor::new(wire.clone());
        let sync_head = SumHead::read(&mut sync_cur).unwrap();
        assert_eq!(sync_head, head, "sync sum_head round-trip mismatch");
        let sync_consumed = sync_cur.position();

        for chunk in [1usize, 2, 3, 7, 13] {
            let mut reader = ChunkedReader::new(wire.clone(), chunk);
            let async_head = SumHead::read_async(&mut reader).await.unwrap();
            assert_eq!(
                async_head, sync_head,
                "async sum_head diverged from sync at chunk {chunk}"
            );
            assert_eq!(
                reader.consumed(),
                sync_consumed,
                "async sum_head consumed a different byte count at chunk {chunk}"
            );
        }
    }
}
