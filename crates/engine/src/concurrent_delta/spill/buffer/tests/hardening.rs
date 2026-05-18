//! Hardening tests for ENOSPC, temp-dir vanish, and partial writes.

use std::fs;
use std::io::{self, Write};

use super::super::super::{SpillCodec, SpillError, SpillableReorderBuffer};
use super::{FailingCodec, drain_all};

#[test]
fn enospc_during_spill_propagates_as_io_error() {
    // Threshold is tiny so the very next insert must spill. The codec
    // returns ENOSPC, simulating the kernel rejecting the spill write.
    let mut buf: SpillableReorderBuffer<FailingCodec> = SpillableReorderBuffer::new(8, 16);
    let healthy = FailingCodec {
        value: 0,
        size: 8,
        fail_kind: None,
    };
    let healthy2 = FailingCodec {
        value: 1,
        size: 16,
        fail_kind: None,
    };
    let poison = FailingCodec {
        value: 99,
        size: 64,
        fail_kind: Some(io::ErrorKind::StorageFull),
    };

    // Seed two healthy items so the spill candidate set is non-empty.
    buf.insert(0, healthy).unwrap();
    buf.insert(1, healthy2).unwrap();

    // Inserting the poisoned item pushes us over the threshold and the
    // codec rejects with ENOSPC during the spill write.
    let err = buf
        .insert(2, poison)
        .expect_err("ENOSPC must surface as an error");

    match err {
        SpillError::Io(ref e) => assert_eq!(e.kind(), io::ErrorKind::StorageFull),
        SpillError::Capacity(_) => panic!("expected I/O error, got capacity"),
    }
    assert!(err.is_out_of_space(), "is_out_of_space should be true");
}

#[test]
fn partial_write_surfaces_as_write_zero() {
    // A writer that accepts one byte and then returns zero models the
    // ENOSPC-mid-record case the std library surfaces as `WriteZero`
    // through the `Write::write_all` contract.
    struct OneByteWriter {
        wrote: bool,
    }
    impl Write for OneByteWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            if self.wrote {
                Ok(0)
            } else {
                self.wrote = true;
                Ok(1)
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = OneByteWriter { wrote: false };
    let codec = FailingCodec {
        value: 7,
        size: 64,
        fail_kind: None,
    };
    let err = codec
        .encode(&mut writer)
        .expect_err("partial write must surface");
    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
}

#[test]
fn temp_dir_vanish_recreates_when_no_prior_spills() {
    // Vanish-before-first-spill is the recoverable case: no data has
    // been written yet, so re-creating the directory and retrying
    // is safe.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    // Operator wipes the spill directory before any spill happens.
    fs::remove_dir_all(&spill_dir).expect("remove spill dir");
    assert!(!spill_dir.exists());

    // These inserts trigger spills. The first spill finds the dir
    // missing, recreates it once, and retries successfully.
    buf.insert(0, 100).unwrap();
    buf.insert(1, 200).unwrap();
    buf.insert(2, 300).unwrap();

    let stats = buf.spill_stats();
    assert_eq!(
        stats.dir_recreate_events, 1,
        "expected exactly one dir recreate, got {}",
        stats.dir_recreate_events
    );
    assert!(spill_dir.exists(), "spill dir should be back");
    assert!(stats.spill_events > 0, "spill must have occurred");
}

#[test]
fn temp_dir_vanish_after_prior_spills_returns_error() {
    // Vanish after prior spills is unrecoverable: those items live
    // only on the now-missing disk. We surface the I/O error rather
    // than silently lose them.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    // Prime the buffer with at least one successful spill.
    buf.insert(0, 100).unwrap();
    buf.insert(1, 200).unwrap();
    assert!(buf.spill_stats().spilled_items > 0);

    // Operator wipes the spill directory mid-transfer. Drop the stale
    // file handle so the next write opens a fresh tempfile and observes
    // the missing parent.
    buf.spill_file = None;
    fs::remove_dir_all(&spill_dir).expect("remove spill dir");

    // The next insert that triggers a spill should surface NotFound
    // (or another io::Error) without panicking and without recreating
    // the directory: prior items are unrecoverable.
    let mut saw_error = false;
    for i in 2u64..6 {
        if let Err(e) = buf.insert(i, i * 100) {
            assert!(matches!(e, SpillError::Io(_)), "expected I/O error");
            saw_error = true;
            break;
        }
    }
    assert!(saw_error, "expected spill failure after dir vanish");
    assert_eq!(
        buf.spill_stats().dir_recreate_events,
        0,
        "must not silently recreate when prior items exist"
    );
}

#[test]
fn dir_recreate_failure_surfaces_io_error() {
    // Point the spill dir at a path whose parent is a regular file:
    // create_dir_all is guaranteed to fail with NotADirectory or similar.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let blocker = scratch.path().join("blocker");
    fs::write(&blocker, b"not a directory").expect("write blocker file");
    let invalid_dir = blocker.join("spill");

    // with_spill_dir performs the first create_dir_all eagerly. The
    // failure must surface cleanly rather than panicking.
    let err = SpillableReorderBuffer::<u64>::with_spill_dir(8, 8, &invalid_dir)
        .expect_err("expected create_dir_all to fail");
    // Different platforms map "parent is a file" to different ErrorKinds
    // (NotADirectory on modern Linux, Other on older toolchains, sometimes
    // AlreadyExists on macOS); any io::Error meets the contract.
    let _ = err.kind();
}

#[test]
fn directory_backed_spill_round_trip() {
    // Sanity: the directory backend yields the same byte-for-byte
    // results as the default spooled backend.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(64, 24, scratch.path().join("spill"))
            .expect("setup spill directory");

    for i in (0..16).rev() {
        buf.insert(i, i * 11).unwrap();
    }
    let items = drain_all(&mut buf);
    let expected: Vec<u64> = (0..16).map(|i| i * 11).collect();
    assert_eq!(items, expected);
    assert!(buf.spill_stats().spill_events > 0);
}
