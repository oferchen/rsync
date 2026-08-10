//! Read-back (reload) fault tests for [`SpillableReorderBuffer`].
//!
//! The spill-to-disk write path is exercised extensively by
//! `enospc_degradation`, `hardening`, and `fault`. This suite covers the
//! mirror-image restore path: what happens when the on-disk record can no
//! longer be read back in full. Every case here proves the same
//! data-integrity contract - a reload that cannot recover the spilled bytes
//! must surface a typed [`SpillError`] loudly, never return `Ok(None)` and
//! silently pretend the reordered block was delivered.
//!
//! Three edges are covered:
//!
//! - The backing spill file handle is gone while the in-memory index still
//!   references a spilled sequence (`reload_item` hits the "not initialized"
//!   guard).
//! - A per-item record on disk is truncated so its payload read runs past
//!   EOF (`reload_item` short read).
//! - A whole-batch record on disk is truncated so its packed-payload read
//!   runs past EOF (`reload_batch` short read).

use std::io::{self, SeekFrom};

use super::super::super::super::reorder::ReorderBuffer;
use super::super::super::{SpillError, SpillGranularity, SpillableReorderBuffer};
use super::super::SPILL_TAG_RAW;

/// Asserts the reload surfaced an I/O failure of the expected kind rather
/// than a silent `Ok(None)`. A silent drop would leave the buffer claiming
/// in-order delivery is complete while a reordered block was actually lost -
/// exactly the corruption these tests exist to forbid.
fn assert_io_kind(err: &SpillError, want: io::ErrorKind) {
    match err {
        SpillError::Io(e) => assert_eq!(
            e.kind(),
            want,
            "expected reload to surface {want:?}, got {:?}",
            e.kind()
        ),
        other => panic!("expected SpillError::Io({want:?}), got {other:?}"),
    }
}

#[test]
fn reload_with_missing_spill_file_surfaces_not_found() {
    // The spill index still points at a spilled sequence but the backing
    // file handle has been dropped (operator cleanup, container restart,
    // handle reset). The reload must fail loudly with NotFound instead of
    // returning Ok(None) and silently dropping the sequence.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(16, 8);

    // Fabricate the "item lives on disk" state with no open backend.
    buf.spill_file = None;
    buf.spill_index.insert(0, 0);

    let err = buf
        .next_in_order()
        .expect_err("reload without a spill file must surface an error, not Ok(None)");
    assert_io_kind(&err, io::ErrorKind::NotFound);
}

#[test]
fn reload_truncated_per_item_record_surfaces_unexpected_eof() {
    // A per-item record whose header advertises an 8-byte payload but whose
    // payload bytes never made it to disk (truncated tempfile) must surface
    // UnexpectedEof on restore. Silently returning the partial bytes would
    // feed the decoder garbage; returning Ok(None) would drop the block.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir)
            .expect("setup spill directory")
            .with_granularity(SpillGranularity::PerItem);

    // Force a real spill so the directory backend file is open.
    for i in (0..6).rev() {
        buf.insert(i, i).expect("seed insert must succeed");
    }
    assert!(
        buf.spill_stats().spill_events > 0,
        "test precondition: at least one item must spill to disk"
    );

    // Append a deliberately truncated per-item record at end-of-file:
    // `[tag][u32 len=8]` with zero payload bytes. reload_item reads the tag
    // and length, then a read_exact of the 8-byte payload hits EOF.
    let offset = {
        let backend = buf.spill_file.as_mut().expect("spill file must be open");
        let file = backend.file();
        let end = file.seek(SeekFrom::End(0)).expect("seek to end");
        file.write_all(&[SPILL_TAG_RAW]).expect("write tag");
        file.write_all(&8u32.to_le_bytes())
            .expect("write length prefix");
        file.flush().expect("flush truncated record");
        end
    };

    // Reset the delivery cursor and point sequence 0 at the truncated record
    // through the single-item (non-batch) reload path.
    buf.inner = ReorderBuffer::new(buf.inner.capacity());
    buf.spill_index.clear();
    buf.batch_members.clear();
    buf.spill_index.insert(0, offset);

    let err = buf
        .next_in_order()
        .expect_err("a short read on restore must surface an error, not Ok(None)");
    assert_io_kind(&err, io::ErrorKind::UnexpectedEof);
}

#[test]
fn reload_truncated_whole_batch_record_surfaces_unexpected_eof() {
    // A whole-batch record whose header advertises a 16-byte packed payload
    // but whose payload bytes are missing must surface UnexpectedEof through
    // the reload_batch path. This is the batch-format mirror of the per-item
    // truncation case above.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir)
            .expect("setup spill directory")
            .with_granularity(SpillGranularity::WholeBatch);

    for i in (0..6).rev() {
        buf.insert(i, i).expect("seed insert must succeed");
    }
    assert!(
        buf.spill_stats().spill_events > 0,
        "test precondition: at least one batch must spill to disk"
    );

    // Append a truncated batch record: `[u32 total_len=16]` with zero
    // payload bytes. reload_batch reads the length, then a read_exact of the
    // 16-byte packed payload hits EOF.
    let offset = {
        let backend = buf.spill_file.as_mut().expect("spill file must be open");
        let file = backend.file();
        let end = file.seek(SeekFrom::End(0)).expect("seek to end");
        file.write_all(&16u32.to_le_bytes())
            .expect("write batch length prefix");
        file.flush().expect("flush truncated record");
        end
    };

    // Route sequence 0 through the batch reload path: spill_index locates the
    // record and batch_members marks it as a one-member batch.
    buf.inner = ReorderBuffer::new(buf.inner.capacity());
    buf.spill_index.clear();
    buf.batch_members.clear();
    buf.spill_index.insert(0, offset);
    buf.batch_members.insert(offset, vec![Some(0)]);

    let err = buf
        .next_in_order()
        .expect_err("a short read on a batch record must surface an error, not Ok(None)");
    assert_io_kind(&err, io::ErrorKind::UnexpectedEof);
}

#[test]
fn drain_ready_propagates_reload_short_read() {
    // The higher-level drain_ready API loops next_in_order; a reload short
    // read must propagate out of it as Err rather than terminating the drain
    // early with a silently-truncated result vector.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir)
            .expect("setup spill directory")
            .with_granularity(SpillGranularity::PerItem);

    for i in (0..6).rev() {
        buf.insert(i, i).expect("seed insert must succeed");
    }

    let offset = {
        let backend = buf.spill_file.as_mut().expect("spill file must be open");
        let file = backend.file();
        let end = file.seek(SeekFrom::End(0)).expect("seek to end");
        file.write_all(&[SPILL_TAG_RAW]).expect("write tag");
        file.write_all(&8u32.to_le_bytes())
            .expect("write length prefix");
        file.flush().expect("flush truncated record");
        end
    };

    buf.inner = ReorderBuffer::new(buf.inner.capacity());
    buf.spill_index.clear();
    buf.batch_members.clear();
    buf.spill_index.insert(0, offset);

    let err = buf
        .drain_ready()
        .expect_err("drain_ready must propagate a reload short read");
    assert_io_kind(&err, io::ErrorKind::UnexpectedEof);
}
