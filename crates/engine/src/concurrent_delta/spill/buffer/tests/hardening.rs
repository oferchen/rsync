//! Hardening tests for ENOSPC, temp-dir vanish, permission denied, and
//! partial writes.

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
        SpillError::UnsupportedCompression(tag) => {
            panic!("expected I/O error, got unsupported compression tag 0x{tag:02x}")
        }
        SpillError::PriorSpillsLost { dir, count } => {
            panic!("expected I/O error, got prior-spills-lost {dir:?} count={count}")
        }
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
    // only on the now-missing disk. We surface the typed
    // PriorSpillsLost variant so the receiver can emit an actionable
    // diagnostic instead of a generic NotFound.
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

    // The next insert that triggers a spill should surface
    // PriorSpillsLost without panicking and without recreating the
    // directory: prior items are unrecoverable.
    let mut saw_error = false;
    for i in 2u64..6 {
        if let Err(e) = buf.insert(i, i * 100) {
            match e {
                SpillError::PriorSpillsLost { ref dir, count } => {
                    assert_eq!(dir, &spill_dir, "variant must carry the vanished dir");
                    assert!(count >= 1, "expected at least one lost chunk, got {count}");
                }
                other => panic!("expected PriorSpillsLost, got {other:?}"),
            }
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
fn prior_spills_lost_surfaces_typed_variant_on_dir_wipe() {
    // SPL-37: a wipe of the spill directory after items are already on
    // disk must surface PriorSpillsLost with the configured dir and a
    // non-zero count of unrecoverable chunks. Forces enough inserts to
    // trip the byte threshold so spill_excess writes real records, then
    // wipes the directory and drives the reload-or-spill path until the
    // typed variant surfaces.
    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(32, 16, &spill_dir).expect("setup spill directory");

    // Seed enough items to comfortably exceed the 16-byte threshold so
    // spill_excess fires and persists at least one record on disk.
    for i in 0u64..6 {
        buf.insert(i, i * 17).expect("seed insert must succeed");
    }
    let seeded = buf.spill_stats().spilled_items;
    assert!(
        seeded >= 1,
        "expected at least one item on disk before wipe, got {seeded}"
    );

    // Wipe the directory mid-transfer. Drop the cached file handle so the
    // next write reopens against the missing parent.
    buf.spill_file = None;
    fs::remove_dir_all(&spill_dir).expect("remove spill dir");

    let mut surfaced: Option<SpillError> = None;
    for i in 6u64..32 {
        match buf.insert(i, i * 23) {
            Ok(()) => continue,
            Err(e) => {
                surfaced = Some(e);
                break;
            }
        }
    }
    let err = surfaced.expect("expected spill to surface an error after wipe");
    match err {
        SpillError::PriorSpillsLost { dir, count } => {
            assert_eq!(dir, spill_dir, "variant must carry the vanished directory");
            assert!(count >= 1, "expected count >= 1, got {count}");
        }
        other => panic!("expected PriorSpillsLost, got {other:?}"),
    }
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

#[cfg(unix)]
#[test]
fn permission_denied_on_spill_dir_surfaces_io_error() {
    // EACCES: the spill directory exists but the process cannot create files
    // inside it. The tempfile creation in open_backend must propagate the
    // PermissionDenied error through write_record -> spill_item ->
    // spill_excess -> insert as SpillError::Io, never panic.
    use std::os::unix::fs::PermissionsExt;

    // Root bypasses file permission checks, so this test is meaningless
    // when running as uid 0. Skip gracefully.
    if rustix::process::getuid().is_root() {
        return;
    }

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    // Revoke all permissions on the spill directory so tempfile_in fails
    // with PermissionDenied on the next spill attempt.
    fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o000))
        .expect("chmod 000 spill dir");

    // Drive enough inserts to trigger a spill. The spill opens a fresh
    // tempfile inside the now-unwritable directory and must surface
    // SpillError::Io(PermissionDenied) cleanly.
    let mut surfaced: Option<SpillError> = None;
    for i in 0u64..8 {
        match buf.insert(i, i * 41) {
            Ok(()) => continue,
            Err(e) => {
                surfaced = Some(e);
                break;
            }
        }
    }

    // Restore permissions before assertions so cleanup succeeds.
    let _ = fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o755));

    let err = surfaced.expect("permission-denied spill dir must surface an error");
    match err {
        SpillError::Io(ref e) => assert_eq!(
            e.kind(),
            io::ErrorKind::PermissionDenied,
            "expected PermissionDenied, got {:?}",
            e.kind()
        ),
        other => panic!("expected SpillError::Io(PermissionDenied), got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn spill_dir_read_only_between_spills_surfaces_io_error() {
    use std::os::unix::fs::PermissionsExt;

    if rustix::process::getuid().is_root() {
        return;
    }

    let scratch = ::tempfile::tempdir().expect("create scratch root");
    let spill_dir = scratch.path().join("spill");
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir).expect("setup spill directory");

    buf.insert(0, 100).unwrap();
    buf.insert(1, 200).unwrap();
    assert!(
        buf.spill_stats().spill_events > 0,
        "expected at least one successful spill"
    );

    buf.spill_file = None;
    fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o444))
        .expect("chmod 444 spill dir");

    let mut surfaced: Option<SpillError> = None;
    for i in 2u64..10 {
        match buf.insert(i, i * 41) {
            Ok(()) => continue,
            Err(e) => {
                surfaced = Some(e);
                break;
            }
        }
    }

    let _ = fs::set_permissions(&spill_dir, fs::Permissions::from_mode(0o755));

    let err = surfaced.expect("read-only spill dir must surface an error on re-open");
    match err {
        SpillError::Io(ref e) => assert_eq!(
            e.kind(),
            io::ErrorKind::PermissionDenied,
            "expected PermissionDenied, got {:?}",
            e.kind()
        ),
        other => panic!("expected SpillError::Io(PermissionDenied), got {other:?}"),
    }
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
