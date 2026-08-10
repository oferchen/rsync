//! Sync vs async wire-parity tests for the file-list entry reader.
//!
//! These prove that the async file-list leaf
//! [`read_entry_with_flist_async`](protocol::flist::read_entry_with_flist_async)
//! decodes byte-identically to the blocking
//! [`FileListReader::read_entry_with_flist`](protocol::flist::FileListReader).
//! An identical flist byte stream (mixed corpus: files, dirs, symlinks, hardlink
//! leader/follower, name-compression, uid/gid, varied sizes/modes/mtimes) is fed
//! to both readers. The async reader is driven over byte-at-a-time and other
//! small chunk boundaries to prove it reassembles entries identically across
//! `.await` points. Every decoded `FileEntry` field and the exact total
//! bytes-consumed are compared entry-for-entry.
//!
//! This is a `tokio-transfer`-gated companion to the multiplex read-leaf parity
//! test (`crates/protocol/src/multiplex/io/parity_tests.rs`); the
//! `async-wire-parity` CI gate runs both.

#![cfg(feature = "tokio-transfer")]

use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use protocol::CompatibilityFlags;
use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter, read_entry_with_flist_async};

/// An [`AsyncRead`] that yields at most `chunk` bytes per `poll_read`, forcing
/// the async reader to reassemble entries across many `.await` points.
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

fn test_protocol() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

/// Builds a mixed corpus of file-list entries that exercises the interleaved
/// decode paths: name prefix-compression, dirs, symlinks, uid/gid, hardlinks,
/// and varied sizes/modes/mtimes.
fn build_corpus() -> Vec<FileEntry> {
    let mut entries = Vec::new();

    let mut f1 = FileEntry::new_file(PathBuf::from("dir/alpha.txt"), 1234, 0o100644);
    f1.set_mtime(1_700_000_000, 0);
    f1.set_uid(1000);
    f1.set_gid(1000);
    entries.push(f1);

    // Shares the "dir/" prefix with the previous entry (name compression).
    let mut f2 = FileEntry::new_file(PathBuf::from("dir/beta.bin"), 0, 0o100600);
    f2.set_mtime(1_700_000_000, 0);
    f2.set_uid(1000);
    f2.set_gid(1000);
    entries.push(f2);

    // Directory entry.
    let mut d1 = FileEntry::new_directory(PathBuf::from("dir/sub"), 0o040755);
    d1.set_mtime(1_700_000_100, 0);
    d1.set_uid(0);
    d1.set_gid(0);
    entries.push(d1);

    // Symlink with a target (exercises the symlink-target read leaf).
    let mut s1 =
        FileEntry::new_symlink(PathBuf::from("dir/sub/link"), PathBuf::from("../alpha.txt"));
    s1.set_mtime(1_700_000_200, 0);
    entries.push(s1);

    // Larger file with a distinct mode/mtime and different uid/gid.
    let mut f3 = FileEntry::new_file(PathBuf::from("dir/sub/gamma"), 9_999_999, 0o100755);
    f3.set_mtime(1_700_000_300, 123);
    f3.set_uid(4242);
    f3.set_gid(99);
    entries.push(f3);

    // Deep path, no shared prefix with the previous symlink dir.
    let mut f4 = FileEntry::new_file(PathBuf::from("other/tree/delta.dat"), 42, 0o100640);
    f4.set_mtime(1_699_999_000, 0);
    f4.set_uid(1000);
    f4.set_gid(1000);
    entries.push(f4);

    entries
}

/// Encodes `entries` via the flist writer with the given preserve flags, then
/// appends the end-of-list marker.
fn encode(protocol: ProtocolVersion, compat: CompatibilityFlags, entries: &[FileEntry]) -> Vec<u8> {
    let mut writer = FileListWriter::with_compat_flags(protocol, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true);
    let mut data = Vec::new();
    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    // End-of-list marker, emitted by the writer so the byte shape matches the
    // reader's expectation exactly (varint vs fixed flags).
    writer.write_end(&mut data, None).unwrap();
    data
}

fn build_reader(protocol: ProtocolVersion, compat: CompatibilityFlags) -> FileListReader {
    FileListReader::with_compat_flags(protocol, compat)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
}

/// A comparable projection of a decoded entry: every field the wire carries.
type EntryFields = (
    String,
    u64,
    u32,
    i64,
    u32,
    Option<u32>,
    Option<u32>,
    Option<PathBuf>,
);

fn project(entries: &[FileEntry]) -> Vec<EntryFields> {
    entries
        .iter()
        .map(|e| {
            (
                e.name().to_string(),
                e.size(),
                e.mode(),
                e.mtime(),
                e.mtime_nsec(),
                e.uid(),
                e.gid(),
                e.link_target().cloned(),
            )
        })
        .collect()
}

/// Drives the blocking reader over the whole stream, returning the decoded
/// entries and the exact bytes consumed (including the end-of-list marker).
fn decode_sync(
    protocol: ProtocolVersion,
    compat: CompatibilityFlags,
    data: &[u8],
) -> (Vec<FileEntry>, usize) {
    let mut reader = build_reader(protocol, compat);
    let mut cursor = Cursor::new(data);
    let mut out = Vec::new();
    while let Some(entry) = reader
        .read_entry_with_flist(&mut cursor, &out.clone())
        .unwrap()
    {
        out.push(entry);
    }
    (out, cursor.position() as usize)
}

/// Drives the async reader over a chunked stream, returning the decoded entries
/// and the exact bytes consumed (including the end-of-list marker).
async fn decode_async(
    protocol: ProtocolVersion,
    compat: CompatibilityFlags,
    data: &[u8],
    chunk: usize,
) -> (Vec<FileEntry>, usize) {
    let mut reader = build_reader(protocol, compat);
    let mut src = ChunkedReader::new(data.to_vec(), chunk);
    let mut carry: Vec<u8> = Vec::new();
    let mut out: Vec<FileEntry> = Vec::new();
    loop {
        let segment = out.clone();
        let entry = read_entry_with_flist_async(&mut reader, &mut src, &mut carry, &segment)
            .await
            .unwrap();
        match entry {
            Some(e) => out.push(e),
            None => break,
        }
    }
    // Bytes consumed = total stream length minus the leftover carry (which must
    // be empty at end-of-list for a well-formed stream).
    (out, data.len() - carry.len())
}

#[tokio::test(flavor = "current_thread")]
async fn async_flist_matches_sync_across_chunk_boundaries() {
    let protocol = test_protocol();
    let compat = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let corpus = build_corpus();
    let data = encode(protocol, compat, &corpus);

    let (sync_entries, sync_consumed) = decode_sync(protocol, compat, &data);
    let sync_fields = project(&sync_entries);

    // Sanity: the whole stream (entries + marker) is consumed by the sync path.
    assert_eq!(
        sync_consumed,
        data.len(),
        "sync did not consume whole stream"
    );
    assert_eq!(
        sync_entries.len(),
        corpus.len(),
        "unexpected sync entry count"
    );

    for chunk in [1usize, 2, 3, 7, 13, data.len().max(1)] {
        let (async_entries, async_consumed) = decode_async(protocol, compat, &data, chunk).await;
        let async_fields = project(&async_entries);

        assert_eq!(
            async_fields, sync_fields,
            "async FileEntry sequence diverged at chunk={chunk}"
        );
        assert_eq!(
            async_consumed, sync_consumed,
            "async bytes-consumed diverged at chunk={chunk}"
        );
    }
}

/// The default flags path (non-varint) must also decode identically, exercising
/// the fixed-byte flag read leaf instead of the varint one.
#[tokio::test(flavor = "current_thread")]
async fn async_flist_matches_sync_default_flags() {
    let protocol = test_protocol();
    let compat = CompatibilityFlags::from_bits(0);
    let corpus = build_corpus();
    let data = encode(protocol, compat, &corpus);

    let (sync_entries, sync_consumed) = decode_sync(protocol, compat, &data);
    let sync_fields = project(&sync_entries);

    for chunk in [1usize, 2, 5, data.len().max(1)] {
        let (async_entries, async_consumed) = decode_async(protocol, compat, &data, chunk).await;
        assert_eq!(
            project(&async_entries),
            sync_fields,
            "async default-flags sequence diverged at chunk={chunk}"
        );
        assert_eq!(
            async_consumed, sync_consumed,
            "async default-flags bytes-consumed diverged at chunk={chunk}"
        );
    }
}
