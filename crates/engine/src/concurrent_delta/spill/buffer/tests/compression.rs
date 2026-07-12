//! [`SpillCompression`] tag round-trip tests.

use std::io::SeekFrom;

#[cfg(not(feature = "spill-compression"))]
use super::super::super::super::reorder::ReorderBuffer;
#[cfg(not(feature = "spill-compression"))]
use super::super::super::SpillError;
use super::super::super::{SpillCodec, SpillCompression, SpillGranularity, SpillableReorderBuffer};
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

/// Walks every `[tag][u32 len][payload]` record in a PerItem spill file up to
/// the current write cursor and returns each record's `(tag, on_disk_len)`.
///
/// Order-independent so a test can assert *which* codecs the compressibility
/// gate selected across a whole workload without pinning a specific record to
/// a specific offset.
#[cfg(feature = "spill-compression")]
fn collect_records<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> Vec<(u8, u32)> {
    let end = buf.spill_write_pos;
    let backend = buf
        .spill_file
        .as_mut()
        .expect("spill file should be initialized");
    let file = backend.file();
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos < end {
        file.seek(SeekFrom::Start(pos)).expect("seek to record");
        let mut header = [0u8; 5];
        file.read_exact(&mut header).expect("read record header");
        let len = u32::from_le_bytes(header[1..5].try_into().unwrap());
        out.push((header[0], len));
        pos += 5 + len as u64;
    }
    out
}

#[test]
fn compression_none_writes_uncompressed_tag() {
    // Default policy: every spill record must start with SPILL_TAG_RAW
    // (0x00) so a default-build reader can decode the payload as-is.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    // PerItem granularity writes a `[tag][u32 len][payload]` header per
    // record, which is what this tag round-trip test inspects. The
    // default WholeBatch granularity omits the per-record tag (the
    // batch header is just `[u32 len][packed payloads]`), so the tag
    // assertion below is only meaningful in PerItem mode.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, scratch.path().join("spill"))
            .expect("setup spill directory")
            .with_compression(SpillCompression::None)
            .with_granularity(SpillGranularity::PerItem);

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
fn compression_zstd_small_record_stays_raw_under_gate() {
    // An 8-byte u64 payload is below the compressibility gate
    // (SPILL_MIN_COMPRESS_BYTES = 64), so even under the Zstd policy the
    // record is stored with SPILL_TAG_RAW (0x00) - zstd's frame overhead
    // could never make an 8-byte record smaller, so paying the codec would
    // be pure waste. The round-trip must still recover the original values.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, scratch.path().join("spill"))
            .expect("setup spill directory")
            .with_compression(SpillCompression::Zstd { level: 1 })
            .with_granularity(SpillGranularity::PerItem);

    for i in (0..6).rev() {
        buf.insert(i, i * 17).unwrap();
    }
    assert!(buf.spill_stats().spill_events > 0, "expected spilling");

    let (tag, len) = read_first_header(&mut buf);
    assert_eq!(
        tag, SPILL_TAG_RAW,
        "a sub-gate record must stay raw even under the Zstd policy"
    );
    assert_eq!(len, 8, "raw u64 payload is 8 bytes");

    let items = drain_all(&mut buf);
    let expected: Vec<u64> = (0..6).map(|i| i * 17).collect();
    assert_eq!(items, expected, "round-trip must preserve values");
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
    // PerItem granularity, same reasoning as the round-trip tests
    // above: only PerItem records carry the per-record tag this test
    // rewrites to assert the reader rejects an unknown compression tag.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir)
            .expect("setup spill directory")
            .with_granularity(SpillGranularity::PerItem);

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

/// Encodes a [`DeltaResult`] to bytes for byte-identical round-trip assertions.
fn encode_result(item: &crate::concurrent_delta::DeltaResult) -> Vec<u8> {
    let mut buf = Vec::new();
    item.encode(&mut buf).expect("encode into Vec cannot fail");
    buf
}

/// Builds a workload of `DeltaResult` items sized so the highest sequences
/// spill first, and drives it through a PerItem spill buffer with the given
/// compression policy. Returns the buffer (spill file still populated) and the
/// original items keyed by sequence for byte-identical comparison after drain.
fn drive_peritem(
    dir: &std::path::Path,
    compression: SpillCompression,
    items: Vec<crate::concurrent_delta::DeltaResult>,
) -> (
    SpillableReorderBuffer<crate::concurrent_delta::DeltaResult>,
    Vec<Vec<u8>>,
) {
    let expected: Vec<Vec<u8>> = items.iter().map(encode_result).collect();
    let mut buf: SpillableReorderBuffer<crate::concurrent_delta::DeltaResult> =
        SpillableReorderBuffer::with_spill_dir(64, 128, dir)
            .expect("setup spill directory")
            .with_compression(compression)
            .with_granularity(SpillGranularity::PerItem);
    // Insert highest-sequence first so the furthest-from-delivery items are
    // the ones evicted to disk.
    for (seq, item) in items.into_iter().enumerate().rev() {
        buf.insert(seq as u64, item).expect("insert must not fail");
    }
    (buf, expected)
}

/// A small `Success` record (37 bytes encoded) is below the compressibility
/// gate, so even under the Zstd policy it is stored raw - no wasted
/// compress+decompress round-trip - and still round-trips byte-identically.
#[cfg(feature = "spill-compression")]
#[test]
fn zstd_gate_small_records_stay_raw() {
    use crate::concurrent_delta::DeltaResult;

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let items: Vec<DeltaResult> = (0..40u32)
        .map(|i| DeltaResult::success(i, u64::from(i) * 10, 5, 5).with_sequence(u64::from(i)))
        .collect();
    let (mut buf, expected) = drive_peritem(
        &scratch.path().join("spill"),
        SpillCompression::Zstd { level: 3 },
        items,
    );

    assert!(buf.spill_stats().spill_events > 0, "expected spilling");
    let records = collect_records(&mut buf);
    assert!(!records.is_empty(), "expected at least one spilled record");
    assert!(
        records.iter().all(|(tag, _)| *tag == SPILL_TAG_RAW),
        "small records must stay raw under the compressibility gate: {records:?}"
    );

    let drained: Vec<Vec<u8>> = drain_all(&mut buf).iter().map(encode_result).collect();
    assert_eq!(drained, expected, "round-trip must be byte-identical");
}

/// A large, highly compressible record (a redo/error reason of repeated bytes)
/// is above the gate and compresses smaller, so it is stored zstd, shrinks on
/// disk, and still round-trips byte-identically.
#[cfg(feature = "spill-compression")]
#[test]
fn zstd_gate_compresses_large_compressible_records() {
    use crate::concurrent_delta::DeltaResult;

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let items: Vec<DeltaResult> = (0..40u32)
        .map(|i| DeltaResult::failed(i, "y".repeat(400)).with_sequence(u64::from(i)))
        .collect();
    let raw_total: u32 = items.iter().map(|d| encode_result(d).len() as u32).sum();
    let (mut buf, expected) = drive_peritem(
        &scratch.path().join("spill"),
        SpillCompression::Zstd { level: 3 },
        items,
    );

    assert!(buf.spill_stats().spill_events > 0, "expected spilling");
    let records = collect_records(&mut buf);
    assert!(
        records.iter().any(|(tag, _)| *tag == SPILL_TAG_ZSTD),
        "compressible records must be stored zstd: {records:?}"
    );
    let on_disk: u32 = records.iter().map(|(_, len)| *len).sum();
    let raw_spilled: u32 = raw_total * records.len() as u32 / 40;
    assert!(
        on_disk < raw_spilled,
        "compressed payload bytes ({on_disk}) must be smaller than raw ({raw_spilled})"
    );

    let drained: Vec<Vec<u8>> = drain_all(&mut buf).iter().map(encode_result).collect();
    assert_eq!(drained, expected, "zstd round-trip must be byte-identical");
}

/// Length-prefixed opaque byte record used to feed the gate genuinely
/// incompressible binary payloads (full 0..=255 byte range), which a
/// UTF-8-constrained reason string cannot express.
#[cfg(feature = "spill-compression")]
#[derive(Clone, PartialEq, Debug)]
struct Blob(Vec<u8>);

#[cfg(feature = "spill-compression")]
impl SpillCodec for Blob {
    fn encode(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        w.write_all(&(self.0.len() as u32).to_le_bytes())?;
        w.write_all(&self.0)
    }

    fn decode(r: &mut dyn std::io::Read) -> std::io::Result<Self> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut bytes = vec![0u8; len];
        r.read_exact(&mut bytes)?;
        Ok(Blob(bytes))
    }

    fn estimated_size(&self) -> usize {
        self.0.len() + 4
    }
}

/// A large but incompressible record (high-entropy binary bytes) is above the
/// size gate yet the zstd output is not strictly smaller, so the codec result
/// is discarded and the record is stored raw. Proves the "not strictly
/// smaller => raw" fallback never inflates a spill record, and that a record
/// stored raw under the Zstd policy still round-trips byte-identically.
#[cfg(feature = "spill-compression")]
#[test]
fn zstd_gate_incompressible_large_records_stay_raw() {
    // splitmix64 yields high-quality pseudorandom bytes across the full 0..=255
    // range, so entropy coding cannot recover the ~13-byte zstd frame overhead
    // - the compressed form is always larger than the 128-byte raw payload.
    let blob = |seed: u64| -> Blob {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let bytes: Vec<u8> = (0..16).flat_map(|_| next().to_le_bytes()).collect();
        Blob(bytes)
    };

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let dir = scratch.path().join("spill");
    let items: Vec<Blob> = (0..40u64).map(|i| blob(0xDEAD_0000 ^ i)).collect();
    let expected = items.clone();

    let mut buf: SpillableReorderBuffer<Blob> =
        SpillableReorderBuffer::with_spill_dir(64, 128, &dir)
            .expect("setup spill directory")
            .with_compression(SpillCompression::Zstd { level: 3 })
            .with_granularity(SpillGranularity::PerItem);
    for (seq, item) in items.into_iter().enumerate().rev() {
        buf.insert(seq as u64, item).expect("insert must not fail");
    }

    assert!(buf.spill_stats().spill_events > 0, "expected spilling");
    let records = collect_records(&mut buf);
    assert!(!records.is_empty(), "expected at least one spilled record");
    assert!(
        records.iter().all(|(tag, _)| *tag == SPILL_TAG_RAW),
        "incompressible records must fall back to raw, never inflate: {records:?}"
    );

    let drained = drain_all(&mut buf);
    assert_eq!(
        drained, expected,
        "raw-fallback round-trip must be byte-identical"
    );
}

/// A mixed workload of small and large records drains byte-identically. Runs
/// in both default (None -> all raw) and `spill-compression` (mixed raw/zstd
/// records interleaved in one spill file) builds, guarding the non-negotiable
/// round-trip contract across every branch of the compressibility gate.
#[test]
fn mixed_sizes_roundtrip_byte_identical() {
    use crate::concurrent_delta::DeltaResult;

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let items: Vec<DeltaResult> = (0..48u32)
        .map(|i| match i % 3 {
            0 => DeltaResult::success(i, u64::from(i), 1, 1).with_sequence(u64::from(i)),
            1 => DeltaResult::needs_redo(i, "z".repeat(200)).with_sequence(u64::from(i)),
            _ => DeltaResult::failed(i, "q".repeat(500)).with_sequence(u64::from(i)),
        })
        .collect();

    #[cfg(feature = "spill-compression")]
    let compression = SpillCompression::Zstd { level: 5 };
    #[cfg(not(feature = "spill-compression"))]
    let compression = SpillCompression::None;

    let (mut buf, expected) = drive_peritem(&scratch.path().join("spill"), compression, items);
    assert!(buf.spill_stats().spill_events > 0, "expected spilling");

    let drained: Vec<Vec<u8>> = drain_all(&mut buf).iter().map(encode_result).collect();
    assert_eq!(
        drained, expected,
        "mixed-size round-trip must be byte-identical regardless of per-record codec"
    );
}
