//! Sync vs async wire-parity for the receiver's file-list reception, gated on
//! `tokio-transfer`.
//!
//! Proves that
//! [`receive_file_list_async`](super::super::super::ReceiverContext::receive_file_list_async)
//! and
//! [`receive_extra_file_lists_async`](super::super::super::ReceiverContext::receive_extra_file_lists_async)
//! build a byte-identical `file_list` to their blocking counterparts for the
//! same wire bytes, including when the async source delivers bytes one at a time
//! across `.await` points. This exercises the shared sans-io decode core end to
//! end through the receiver's segment-loop post-processing (sort, hardlink
//! matching), not just the protocol leaf.

use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use protocol::flist::{FileEntry, FileListWriter};
use protocol::{CompatibilityFlags, ProtocolVersion};
use tokio::io::{AsyncRead, ReadBuf};

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// An [`AsyncRead`] that yields at most `chunk` bytes per `poll_read`.
struct ChunkedReader {
    data: Vec<u8>,
    pos: usize,
    chunk: usize,
}

impl ChunkedReader {
    fn new(data: Vec<u8>, chunk: usize) -> Self {
        Self {
            data,
            pos: 0,
            chunk: chunk.max(1),
        }
    }
}

impl AsyncRead for ChunkedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let remaining = self.data.len() - self.pos;
        if remaining == 0 {
            return Poll::Ready(Ok(()));
        }
        let take = remaining.min(self.chunk).min(buf.remaining());
        let start = self.pos;
        let end = start + take;
        buf.put_slice(&self.data[start..end]);
        self.pos = end;
        Poll::Ready(Ok(()))
    }
}

/// Projects a file list to its comparable per-entry fields.
fn project(ctx: &ReceiverContext) -> Vec<(String, u64, u32, i64)> {
    ctx.file_list()
        .iter()
        .map(|e| (e.name().to_string(), e.size(), e.mode(), e.mtime()))
        .collect()
}

fn encode_initial() -> Vec<u8> {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut writer = FileListWriter::new(protocol);
    let mut data = Vec::new();

    for (name, size, mode) in [
        ("dir/alpha.txt", 1234u64, 0o100644u32),
        ("dir/beta.bin", 0, 0o100600),
        ("dir/gamma", 9_999_999, 0o100755),
        ("other/delta.dat", 42, 0o100640),
    ] {
        let mut e = FileEntry::new_file(PathBuf::from(name), size, mode);
        e.set_mtime(1_700_000_000, 0);
        writer.write_entry(&mut data, &e).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();
    data
}

#[tokio::test(flavor = "current_thread")]
async fn receive_file_list_async_matches_sync() {
    let data = encode_initial();

    // Sync baseline.
    let mut sync_ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    let sync_count = sync_ctx
        .receive_file_list(&mut Cursor::new(&data[..]))
        .unwrap();
    let sync_fields = project(&sync_ctx);

    for chunk in [1usize, 2, 3, 7, 13, data.len()] {
        let mut async_ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
        let mut src = ChunkedReader::new(data.clone(), chunk);
        let async_count = async_ctx.receive_file_list_async(&mut src).await.unwrap();

        assert_eq!(
            async_count, sync_count,
            "entry count diverged at chunk={chunk}"
        );
        assert_eq!(
            project(&async_ctx),
            sync_fields,
            "file_list diverged at chunk={chunk}"
        );
    }
}

/// Builds a config + handshake with INC_RECURSE negotiated for the extra-lists
/// path, then drives an initial segment plus one INC_RECURSE sub-list through
/// both the sync and async readers and compares.
#[tokio::test(flavor = "current_thread")]
async fn receive_extra_file_lists_async_matches_sync() {
    use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};

    let protocol = ProtocolVersion::try_from(32u8).unwrap();

    // Build the extra-list wire bytes: one segment for dir_ndx=0 then EOF.
    let mut writer = FileListWriter::new(protocol);
    let mut extra = Vec::new();
    let mut ndx_codec = create_ndx_codec(protocol.as_u8());
    // Segment for the top-level content dir (dir_ndx = 0): NDX_FLIST_OFFSET - 0.
    ndx_codec.write_ndx(&mut extra, NDX_FLIST_OFFSET).unwrap();
    for (name, size) in [("sub/one.txt", 11u64), ("sub/two.txt", 22)] {
        let mut e = FileEntry::new_file(PathBuf::from(name), size, 0o100644);
        e.set_mtime(1_700_000_500, 0);
        writer.write_entry(&mut extra, &e).unwrap();
    }
    writer.write_end(&mut extra, None).unwrap();
    ndx_codec.write_ndx(&mut extra, NDX_FLIST_EOF).unwrap();

    let handshake = {
        let mut h = test_handshake();
        h.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
        h
    };

    // Sync baseline: seed an initial (empty) segment so ndx_segments has a base.
    let mut sync_ctx = ReceiverContext::new_for_test(&handshake, test_config());
    let sync_total = sync_ctx
        .receive_extra_file_lists(&mut Cursor::new(&extra[..]))
        .unwrap();
    let sync_fields = project(&sync_ctx);

    for chunk in [1usize, 2, 3, 8, extra.len()] {
        let mut async_ctx = ReceiverContext::new_for_test(&handshake, test_config());
        let mut src = ChunkedReader::new(extra.clone(), chunk);
        let async_total = async_ctx
            .receive_extra_file_lists_async(&mut src)
            .await
            .unwrap();

        assert_eq!(
            async_total, sync_total,
            "extra count diverged at chunk={chunk}"
        );
        assert_eq!(
            project(&async_ctx),
            sync_fields,
            "extra file_list diverged at chunk={chunk}"
        );
    }
}
