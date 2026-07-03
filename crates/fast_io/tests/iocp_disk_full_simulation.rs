//! Disk-full regression coverage for the Windows IOCP disk-commit path
//! (task #1932).
//!
//! Real disk-full coverage on CI is impractical because Windows lacks a
//! ramfs/tmpfs equivalent and provisioning a VHD just for these tests is
//! heavyweight and flaky on hosted runners. Instead we drive the failure
//! through the `inject_next_write_error_for_test` hook in
//! `crates/fast_io/src/iocp/disk_batch.rs`, which short-circuits the next
//! `WriteFile` submission with a synthetic Win32 error code before any
//! kernel call. This delivers byte-for-byte the same error path that a real
//! ERROR_DISK_FULL (Win32 code 112) would produce, just deterministically.
//!
//! The whole file is cfg-gated to `windows + iocp` so it compiles to
//! nothing on Linux and macOS.

#![cfg(all(target_os = "windows", feature = "iocp"))]

use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;
use std::path::Path;

use tempfile::tempdir;
use windows_sys::Win32::Foundation::{
    ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use fast_io::iocp::{
    IocpConfig, IocpDiskBatch, clear_injected_write_error_for_test,
    inject_next_write_error_for_test, is_iocp_available,
};

/// RAII guard that clears the global fault-injection state on drop so a
/// panicking assertion never leaks countdown into a sibling test.
struct InjectionGuard;

impl Drop for InjectionGuard {
    fn drop(&mut self) {
        clear_injected_write_error_for_test();
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Opens a fresh writable file with sharing flags compatible with the
/// `ReOpenFile` call inside `IocpDiskBatch::begin_file`. Mirrors the helper
/// used by the inline unit tests in `disk_batch.rs`.
fn open_writable(path: &Path) -> File {
    let wide = to_wide(path);
    // SAFETY: Standard Win32 open with a zero-terminated wide string. The
    // returned handle is exclusively owned and wrapped into `File` so Drop
    // closes it.
    #[allow(unsafe_code)]
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "CreateFileW failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: `handle` is fresh and exclusively owned.
    #[allow(unsafe_code)]
    unsafe {
        File::from_raw_handle(handle as *mut std::ffi::c_void)
    }
}

/// Sanity check: on Rust 1.88 the std library maps Win32 ERROR_DISK_FULL
/// (112) and ERROR_HANDLE_DISK_FULL (39) onto `io::ErrorKind::StorageFull`.
/// The transfer-layer error categorizer routes that kind into the fatal
/// `DiskFull` branch, so the IOCP write path inherits the correct
/// classification simply by surfacing the raw OS error.
#[test]
fn std_maps_disk_full_codes_to_storage_full_kind() {
    let primary = io::Error::from_raw_os_error(ERROR_DISK_FULL as i32);
    assert_eq!(primary.kind(), io::ErrorKind::StorageFull);
    assert_eq!(primary.raw_os_error(), Some(ERROR_DISK_FULL as i32));

    let handle_variant = io::Error::from_raw_os_error(ERROR_HANDLE_DISK_FULL as i32);
    assert_eq!(handle_variant.kind(), io::ErrorKind::StorageFull);
    assert_eq!(
        handle_variant.raw_os_error(),
        Some(ERROR_HANDLE_DISK_FULL as i32)
    );
}

/// The first faulted submission inside `flush_current` must surface as a
/// `StorageFull` error with the original Win32 code intact, so downstream
/// `transfer` code can categorize it as fatal without re-decoding. The
/// buffered `write_data` returns success because the data fits in the
/// in-memory buffer; the fault surfaces at the explicit `flush` boundary,
/// which is exactly where the disk-commit thread reaps real WriteFile
/// errors.
#[test]
fn first_submission_disk_full_surfaces_storage_full() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }
    let _guard = InjectionGuard;

    let dir = tempdir().unwrap();
    let path = dir.path().join("first_submission.bin");
    let file = open_writable(&path);

    // Force every flush to issue a single 4 KB chunk so the first
    // submission is also the one that exercises the fault.
    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 1,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    let payload = vec![0xCDu8; 4096];
    batch.write_data(&payload).unwrap();

    // Arm the hook to fault the very next submission with ERROR_DISK_FULL,
    // then trigger the flush that will perform that submission.
    inject_next_write_error_for_test(1, ERROR_DISK_FULL as i32);
    let err = batch
        .flush()
        .expect_err("flush must propagate the injected disk-full error");

    assert_eq!(
        err.kind(),
        io::ErrorKind::StorageFull,
        "raw error 112 must map to StorageFull, got {err:?}"
    );
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_DISK_FULL as i32),
        "raw OS error must round-trip through the IOCP submit path"
    );
}

/// A faulted submission must leave the batch writer in a usable state:
/// dropping it (and the underlying completion port) must not panic or
/// block, and the previously-committed file must remain on disk intact.
#[test]
fn writer_drop_after_disk_full_is_clean() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }
    let _guard = InjectionGuard;

    let dir = tempdir().unwrap();

    // File A: writes succeed and the file is committed before the fault.
    let path_a = dir.path().join("durable.bin");
    let file_a = open_writable(&path_a);

    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 2,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file_a).unwrap();
    batch.write_data(b"persisted-before-failure").unwrap();
    let (_returned_a, written_a) = batch.commit_file(true).unwrap();
    assert_eq!(written_a as usize, b"persisted-before-failure".len());

    // File B: arm the hook to fault on the very next submit. Commit (which
    // calls flush_current) must surface the disk-full error.
    let path_b = dir.path().join("doomed.bin");
    let file_b = open_writable(&path_b);
    batch.begin_file(file_b).unwrap();
    batch.write_data(&vec![0u8; 4096]).unwrap();

    inject_next_write_error_for_test(1, ERROR_DISK_FULL as i32);
    let err = batch
        .commit_file(true)
        .expect_err("commit_file must propagate injected ERROR_DISK_FULL");
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert_eq!(err.raw_os_error(), Some(ERROR_DISK_FULL as i32));

    // Drop the batch explicitly while still inside the test body so any
    // panic inside `Drop` would fail the test rather than abort the runner.
    drop(batch);

    // File A must still be readable and intact: failure on a later file
    // never invalidates earlier commits.
    let content = std::fs::read(&path_a).unwrap();
    assert_eq!(content, b"persisted-before-failure");
}

/// Subsequent batches must continue to work after the injected error has
/// been consumed - the single fault must not poison the global hook or any
/// internal cached state shared by future `IocpDiskBatch` instances.
#[test]
fn batch_recovers_after_injected_fault_consumed() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }
    let _guard = InjectionGuard;

    let dir = tempdir().unwrap();
    let path1 = dir.path().join("fault.bin");
    let file1 = open_writable(&path1);

    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 1,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file1).unwrap();

    batch.write_data(&vec![0u8; 4096]).unwrap();
    inject_next_write_error_for_test(1, ERROR_DISK_FULL as i32);
    let err = batch.flush().expect_err("injected fault must surface");
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);

    // Hook is single-shot. A second batch using a fresh file must succeed
    // end-to-end with no manual `clear` call between cases.
    drop(batch);

    let path2 = dir.path().join("recovered.bin");
    let file2 = open_writable(&path2);
    let mut next = IocpDiskBatch::new(&config).unwrap();
    next.begin_file(file2).unwrap();
    next.write_data(b"recovered").unwrap();
    let (_returned, written) = next.commit_file(false).unwrap();
    assert_eq!(written as usize, b"recovered".len());

    let content = std::fs::read(&path2).unwrap();
    assert_eq!(content, b"recovered");
}

/// The Nth-submit variant: arming the hook with `nth = 3` must let the
/// first two submissions succeed and only fault the third one. This
/// validates that the countdown decrements rather than firing immediately.
#[test]
fn nth_submission_disk_full_skips_earlier_writes() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }
    let _guard = InjectionGuard;

    let dir = tempdir().unwrap();
    let path = dir.path().join("nth.bin");
    let file = open_writable(&path);

    // chunk_size = 4096 with 16 KB of payload yields exactly four
    // submissions per flush. Arm the third one to fail.
    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 1,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    let payload: Vec<u8> = (0..16 * 1024).map(|i| (i & 0xFF) as u8).collect();
    batch.write_data(&payload).unwrap();

    inject_next_write_error_for_test(3, ERROR_DISK_FULL as i32);
    let err = batch
        .flush()
        .expect_err("third submission must surface ERROR_DISK_FULL");
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert_eq!(err.raw_os_error(), Some(ERROR_DISK_FULL as i32));

    // Drop must still be clean after a mid-flush fault. After Drop, the
    // file is closed by `File`'s Drop impl and the on-disk size reflects
    // every chunk that actually reached the kernel before the fault. With
    // max_in_flight=1 the first two chunks (8 KB) drain to disk before the
    // third call invokes the hook, so the file lands at exactly 8 KB.
    drop(batch);

    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(
        metadata.len(),
        8 * 1024,
        "first two chunks (8 KB) must reach disk before the third submission \
         faults, leaving the file at exactly 8 KB"
    );
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(
        on_disk,
        payload[..8 * 1024],
        "pre-fault chunk content must match the source bytes"
    );
}

/// Regression for the mid-batch submission-error use-after-free (#488): with
/// more than one op allowed in flight, a synchronous submit fault must drain
/// the in-flight ops the kernel already accepted before their pinned
/// OVERLAPPED buffers are dropped. Dropping them under an outstanding
/// `WriteFile` would let the kernel write into freed memory and post late
/// completions against freed OVERLAPPED structs.
///
/// The invariant this test encodes: the doomed batch must (a) surface the
/// injected disk-full error, (b) not hang waiting on completions it dropped,
/// (c) drop cleanly, and (d) leave only whole, source-matching chunks on
/// disk. It intentionally uses `concurrent_ops = 2` so `in_flight` holds an
/// outstanding op at the moment the fault fires - the single-op tests above
/// never populate the drain-on-error path because at most one op is ever in
/// flight.
#[test]
fn multi_in_flight_disk_full_drains_before_drop() {
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }
    let _guard = InjectionGuard;

    let dir = tempdir().unwrap();
    let path = dir.path().join("multi_in_flight.bin");
    let file = open_writable(&path);

    // 4 KB chunks, up to two in flight, 16 KB payload => four submissions.
    // Faulting the third leaves at least one earlier op outstanding when the
    // error path drains.
    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 2,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    let chunk = 4096usize;
    let payload: Vec<u8> = (0..4 * chunk).map(|i| (i & 0xFF) as u8).collect();
    batch.write_data(&payload).unwrap();

    inject_next_write_error_for_test(3, ERROR_DISK_FULL as i32);
    let err = batch
        .flush()
        .expect_err("mid-batch submission fault must surface ERROR_DISK_FULL");
    assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    assert_eq!(err.raw_os_error(), Some(ERROR_DISK_FULL as i32));

    // Clean drop: a UAF or an un-drained op would surface as a hang, a panic
    // in Drop, or heap corruption here.
    drop(batch);

    // Only whole, durable chunks may remain, and their bytes must match the
    // source prefix. The exact count is not asserted because completion
    // ordering with two ops in flight is not deterministic; the durable
    // prefix is always a whole number of chunks of the original data.
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(
        on_disk.len() % chunk,
        0,
        "only whole chunks may reach disk before the fault, got {} bytes",
        on_disk.len()
    );
    assert!(
        on_disk.len() <= payload.len(),
        "on-disk size must not exceed the payload"
    );
    assert_eq!(
        on_disk.as_slice(),
        &payload[..on_disk.len()],
        "durable prefix must match the source bytes exactly"
    );
}
