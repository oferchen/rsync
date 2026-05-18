//! [`SpillCompression`] tag round-trip tests.

use std::io::SeekFrom;

#[cfg(not(feature = "spill-compression"))]
use super::super::super::super::reorder::ReorderBuffer;
#[cfg(not(feature = "spill-compression"))]
use super::super::super::SpillError;
use super::super::super::{SpillCodec, SpillCompression, SpillableReorderBuffer};
use super::super::{SPILL_TAG_RAW, SPILL_TAG_ZSTD};
use super::drain_all;

/// Reads the first record header (tag + length) from a spill file at
/// offset zero. Used by the compression tests to inspect the leading
/// tag byte without re-implementing the wire format.
fn read_first_header<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> (u8, u32) {
    let backend = buf
        .spill_file
        .as_mut()
        .expect("spill file should be initialized");
    let file = backend.file();
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    let mut header = [0u8; 5];
    file.read_exact(&mut header).expect("read header");
    let len = u32::from_le_bytes(header[1..5].try_into().unwrap());
    (header[0], len)
}

#[test]
fn compression_none_writes_uncompressed_tag() {
    // Default policy: every spill record must start with SPILL_TAG_RAW
    // (0x00) so a default-build reader can decode the payload as-is.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, scratch.path().join("spill"))
            .expect("setup spill directory")
            .with_compression(SpillCompression::None);

    for i in (0..6).rev() {
        buf.insert(i, i * 13).unwrap();
    }
    assert!(buf.spill_stats().spill_events > 0, "expected spilling");

    let (tag, len) = read_first_header(&mut buf);
    assert_eq!(tag, SPILL_TAG_RAW, "first record must carry the raw tag");
    assert_eq!(len, 8, "u64 payload is 8 bytes uncompressed");

    let items = drain_all(&mut buf);
    let expected: Vec<u64> = (0..6).map(|i| i * 13).collect();
    assert_eq!(items, expected, "round-trip must preserve values");
}

#[cfg(feature = "spill-compression")]
#[test]
fn compression_zstd_writes_compressed_tag() {
    // With the spill-compression feature on, every record must start
    // with SPILL_TAG_ZSTD (0x01) and the round-trip must still recover
    // the original values.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, scratch.path().join("spill"))
            .expect("setup spill directory")
            .with_compression(SpillCompression::Zstd { level: 1 });

    for i in (0..6).rev() {
        buf.insert(i, i * 17).unwrap();
    }
    assert!(buf.spill_stats().spill_events > 0, "expected spilling");

    let (tag, _len) = read_first_header(&mut buf);
    assert_eq!(tag, SPILL_TAG_ZSTD, "first record must carry the zstd tag");

    let items = drain_all(&mut buf);
    let expected: Vec<u64> = (0..6).map(|i| i * 17).collect();
    assert_eq!(items, expected, "zstd round-trip must preserve values");
}

#[cfg(not(feature = "spill-compression"))]
#[test]
fn compression_zstd_tag_without_feature_returns_unsupported() {
    // A default build reading a spill file that advertises the zstd tag
    // (e.g. produced by a spill-compression build sharing a scratch dir)
    // must surface UnsupportedCompression instead of feeding garbage to
    // the codec. The Zstd variant is itself unconstructable here (the
    // `#[cfg]` gate on SpillCompression::Zstd is the compile-time
    // "fail fast at construction" guarantee), so we inject the tag
    // directly into the spill file.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir).expect("setup spill directory");

    // Force an open spill file by triggering one normal spill first.
    for i in (0..4).rev() {
        buf.insert(i, i).unwrap();
    }
    assert!(buf.spill_stats().spill_events > 0, "expected spilling");

    // Overwrite the first record's tag with the zstd marker. The length
    // field after it still reflects the original raw payload, but the
    // reader must reject the record on the tag alone.
    {
        let backend = buf
            .spill_file
            .as_mut()
            .expect("spill file should be initialized");
        let file = backend.file();
        file.seek(SeekFrom::Start(0)).expect("seek to start");
        file.write_all(&[SPILL_TAG_ZSTD]).expect("write tag");
        file.flush().expect("flush tag write");
    }

    // Reset the buffer's delivery cursor so we observe the rewritten
    // record on the next drain attempt.
    buf.inner = ReorderBuffer::new(buf.inner.capacity());

    // Tell the spillable buffer that sequence 0 still lives on disk at
    // offset 0 so next_in_order will read it back through the new tag.
    buf.spill_index.clear();
    buf.spill_index.insert(0, 0);

    let err = buf
        .next_in_order()
        .expect_err("reading a zstd tag without the feature must fail");
    match err {
        SpillError::UnsupportedCompression(tag) => assert_eq!(tag, SPILL_TAG_ZSTD),
        other => panic!("expected UnsupportedCompression, got {other:?}"),
    }
}
