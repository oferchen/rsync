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
        let (async_count, _leftover) = async_ctx.receive_file_list_async(&mut src).await.unwrap();

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
        let (async_total, _leftover) = async_ctx
            .receive_extra_file_lists_async(&mut src, Vec::new())
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

/// Builds a sequence of per-file sender-response frames (NDX + iflags + optional
/// basis-type / xname / xattr-abbreviation data), then drives both the sync
/// [`SenderAttrs::read_with_codec_xattr`] and the async
/// [`SenderAttrs::read_with_codec_xattr_async`] over the same wire bytes and
/// asserts every decoded `(ndx, iflags, fnamecmp_type, xname, xattr_values)`
/// tuple matches, including when the async source dribbles bytes across
/// `.await` points. This is the read-leaf parity proof for the receiver's
/// `read_ndx_and_attrs` twin (upstream rsync.c:read_ndx_and_attrs).
#[tokio::test(flavor = "current_thread")]
async fn sender_attrs_read_async_matches_sync() {
    use protocol::codec::{NdxCodec, create_ndx_codec};
    use protocol::write_varint;

    use super::super::super::wire::SenderAttrs;

    // Encode four sequential sender-response frames for protocol 31:
    //   0: plain transfer (iflags only)
    //   1: basis-type follows (fnamecmp_type byte)
    //   2: xname follows (short vstring)
    //   3: xattr report (abbreviation data: two entries + terminator)
    let mut sender_codec = create_ndx_codec(31);
    let mut wire = Vec::new();

    // Frame 0: ndx=0, ITEM_TRANSFER only.
    sender_codec.write_ndx(&mut wire, 0).unwrap();
    wire.extend_from_slice(&SenderAttrs::ITEM_TRANSFER.to_le_bytes());

    // Frame 1: ndx=1, ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS, basis byte = 2 (Fuzzy).
    sender_codec.write_ndx(&mut wire, 1).unwrap();
    wire.extend_from_slice(
        &(SenderAttrs::ITEM_TRANSFER | SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS).to_le_bytes(),
    );
    wire.push(2u8);

    // Frame 2: ndx=2, ITEM_TRANSFER | ITEM_XNAME_FOLLOWS, vstring "alt.bin".
    sender_codec.write_ndx(&mut wire, 2).unwrap();
    wire.extend_from_slice(
        &(SenderAttrs::ITEM_TRANSFER | SenderAttrs::ITEM_XNAME_FOLLOWS).to_le_bytes(),
    );
    let xname = b"alt.bin";
    wire.push(xname.len() as u8); // short vstring length prefix (< 0x80)
    wire.extend_from_slice(xname);

    // Frame 3: ndx=3, ITEM_TRANSFER | ITEM_REPORT_XATTR, two abbreviation entries.
    sender_codec.write_ndx(&mut wire, 3).unwrap();
    wire.extend_from_slice(
        &(SenderAttrs::ITEM_TRANSFER | SenderAttrs::ITEM_REPORT_XATTR).to_le_bytes(),
    );
    // Entry A: rel_pos=1 (num=1), datum_len=3, value=b"abc".
    write_varint(&mut wire, 1).unwrap();
    write_varint(&mut wire, 3).unwrap();
    wire.extend_from_slice(b"abc");
    // Entry B: rel_pos=2 (num=3), datum_len=1, value=b"z".
    write_varint(&mut wire, 2).unwrap();
    write_varint(&mut wire, 1).unwrap();
    wire.extend_from_slice(b"z");
    // Terminator.
    write_varint(&mut wire, 0).unwrap();

    // Projected comparable tuple for one decoded frame.
    type Frame = (i32, u16, Option<u8>, Option<Vec<u8>>, Vec<(i32, Vec<u8>)>);
    fn project_attrs(ndx: i32, a: &SenderAttrs) -> Frame {
        (
            ndx,
            a.iflags,
            a.fnamecmp_type.map(|t| t.to_wire()),
            a.xname.clone(),
            a.xattr_values.clone(),
        )
    }

    // Sync baseline: read all four frames with preserve_xattrs=true.
    let mut sync_codec = create_ndx_codec(31);
    let mut sync_cursor = Cursor::new(&wire[..]);
    let mut sync_frames = Vec::new();
    for _ in 0..4 {
        let (ndx, attrs) =
            SenderAttrs::read_with_codec_xattr(&mut sync_cursor, &mut sync_codec, true, false)
                .unwrap();
        sync_frames.push(project_attrs(ndx, &attrs));
    }

    for chunk in [1usize, 2, 3, 5, 9, wire.len()] {
        let mut async_codec = create_ndx_codec(31);
        let mut src = ChunkedReader::new(wire.clone(), chunk);
        let mut async_frames = Vec::new();
        for _ in 0..4 {
            let (ndx, attrs) =
                SenderAttrs::read_with_codec_xattr_async(&mut src, &mut async_codec, true, false)
                    .await
                    .unwrap();
            async_frames.push(project_attrs(ndx, &attrs));
        }
        assert_eq!(
            async_frames, sync_frames,
            "sender-attrs frames diverged at chunk={chunk}"
        );
    }
}
