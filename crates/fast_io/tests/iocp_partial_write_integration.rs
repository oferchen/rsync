//! Integration tests for the Windows IOCP write path under simulated memory
//! pressure (task #1931).
//!
//! On Linux a `write(2)` may complete with fewer bytes than requested. On
//! Windows the equivalent overlapped `WriteFile` call can also report a
//! partial completion when the kernel cannot satisfy the full request, for
//! example when paged-pool is constrained, the disk cache is throttled, or
//! the I/O subsystem splits a large request across multiple completion
//! packets. The [`IocpDiskBatch`] path explicitly handles this case by
//! resubmitting the unwritten tail at the appropriate offset
//! (see `submit_write_batch` in `crates/fast_io/src/iocp/disk_batch.rs`).
//!
//! These tests simulate the same pressure pattern with public API only:
//!
//! - Drive a 16-32 MB payload through [`IocpDiskBatch`] with a very small
//!   `buffer_size` and `concurrent_ops` so the writer must issue many
//!   overlapped submissions, drain the completion port multiple times, and
//!   accumulate every byte before reporting success.
//! - Drive the same kind of payload through [`IocpWriter`] with a small
//!   internal buffer so the implicit-flush branch in
//!   `<IocpWriter as Write>::write` runs repeatedly.
//! - Verify total bytes written matches input and the file contents match
//!   byte-for-byte.
//! - Exercise the chunk-boundary case where the writer must walk a
//!   buffer boundary in the middle of a single caller-side `write_all` call,
//!   modelling the "one chunk reports a partial completion - split into two
//!   CQEs" scenario at the API layer.
//! - Exercise the error path: writes against an [`IocpDiskBatch`] with no
//!   active file must surface `InvalidInput`, and `begin_file` on a handle
//!   that cannot be reopened for overlapped writes must surface the
//!   underlying Win32 error rather than silently succeeding.
//!
//! The whole file compiles to nothing on non-Windows targets thanks to the
//! top-level `cfg` gate, matching the gating used by the IOCP module itself.

#![cfg(all(target_os = "windows", feature = "iocp"))]

use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::FromRawHandle;
use std::path::Path;

use fast_io::{IocpConfig, IocpDiskBatch, IocpWriter, is_iocp_available};
use tempfile::tempdir;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// 16 MiB payload: large enough to force many overlapped submissions and
/// many completion drains under the constrained `buffer_size` /
/// `concurrent_ops` chosen below, while small enough to stay well under the
/// disk-space budget of every CI runner. Increase to 32 MiB by setting the
/// `OC_RSYNC_IOCP_PARTIAL_WRITE_BYTES` environment variable when running
/// the test locally for stress profiling.
const PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Deterministic payload pattern. Byte at offset `i` is `(i * 31 + 7) % 251`
/// so adjacent chunks look distinct and a misordered resubmission would
/// surface immediately on byte compare.
fn make_payload(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(((i.wrapping_mul(31).wrapping_add(7)) % 251) as u8);
    }
    out
}

/// Reads the optional `OC_RSYNC_IOCP_PARTIAL_WRITE_BYTES` override. Capped
/// at 64 MiB so a typo cannot exhaust the runner's tmpdir.
fn payload_size() -> usize {
    std::env::var("OC_RSYNC_IOCP_PARTIAL_WRITE_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.min(64 * 1024 * 1024))
        .unwrap_or(PAYLOAD_BYTES)
}

/// Encodes `path` as a NUL-terminated wide string for Win32 APIs.
fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Opens `path` for writing and returns a `File` wrapping the raw handle.
/// Shares read/write/delete so `ReOpenFile` inside the disk batch can
/// acquire a second overlapped handle without `ERROR_SHARING_VIOLATION`.
fn open_writable(path: &Path) -> File {
    let wide = to_wide(path);
    // SAFETY: Standard Win32 open with a NUL-terminated wide path. The
    // returned handle is wrapped in a `File` immediately so Drop closes it.
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
    // SAFETY: `handle` is a fresh, exclusively-owned kernel handle.
    #[allow(unsafe_code)]
    unsafe {
        File::from_raw_handle(handle as *mut std::ffi::c_void)
    }
}

/// Opens `path` for read-only access. Used by the error-path test to feed
/// `IocpDiskBatch::begin_file` a handle that cannot be reopened with
/// `FILE_GENERIC_WRITE`, which forces `ReOpenFile` to fail.
fn open_readonly(path: &Path) -> File {
    let wide = to_wide(path);
    // SAFETY: Standard Win32 open for an existing file with read-only
    // access; the resulting handle is owned by the returned File.
    #[allow(unsafe_code)]
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "CreateFileW (read-only) failed: {}",
        io::Error::last_os_error()
    );
    // SAFETY: handle is freshly opened and exclusively owned here.
    #[allow(unsafe_code)]
    unsafe {
        File::from_raw_handle(handle as *mut std::ffi::c_void)
    }
}

/// Drives a multi-megabyte payload through [`IocpDiskBatch`] using a very
/// small `buffer_size` and shallow `concurrent_ops` so the writer must
/// issue many overlapped submissions and drain the completion port many
/// times. This is the closest model of the memory-pressure scenario without
/// requiring kernel co-operation: each chunk reaches the kernel via a
/// distinct `WriteFile` call and the in-flight queue is intentionally
/// shallow so the batch cannot hide variability behind parallelism.
#[test]
fn disk_batch_accumulates_under_simulated_pressure() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("disk_batch_partial.bin");
    let payload = make_payload(payload_size());

    let config = IocpConfig {
        // Small chunks force the submit loop to fan out many overlapped
        // writes and drain repeatedly. Matches the worst-case shape that
        // surfaces partial completions under memory pressure.
        buffer_size: 4 * 1024,
        // Shallow in-flight queue keeps the batch from hiding latency
        // behind parallelism; every flush requires multiple drain cycles.
        concurrent_ops: 2,
        unbuffered: false,
        write_through: false,
    };

    let mut batch = IocpDiskBatch::new(&config).expect("create IocpDiskBatch");
    let file = open_writable(&path);
    batch.begin_file(file).expect("begin_file");

    // Feed the payload in 64 KiB slabs so the batch sees back-to-back
    // writes that cross its internal buffer boundary repeatedly.
    let slab = 64 * 1024;
    let mut offset = 0;
    while offset < payload.len() {
        let end = (offset + slab).min(payload.len());
        batch
            .write_data(&payload[offset..end])
            .expect("write_data under pressure");
        offset = end;
    }

    let (_returned, written) = batch.commit_file(true).expect("commit_file");
    assert_eq!(
        written as usize,
        payload.len(),
        "IocpDiskBatch must accumulate every byte across the partial-completion loop"
    );

    let on_disk = std::fs::read(&path).expect("read back disk batch output");
    assert_eq!(
        on_disk.len(),
        payload.len(),
        "on-disk size mismatch after IocpDiskBatch commit"
    );
    assert!(
        on_disk == payload,
        "on-disk bytes diverge from input - resubmission likely lost or reordered a chunk"
    );
}

/// Drives the same payload through [`IocpWriter`] with a small internal
/// buffer so the implicit-flush branch of `<IocpWriter as Write>::write`
/// runs on nearly every caller-side `write_all` invocation. This exercises
/// the `flush_buffer` loop that splits the buffer into chunk-sized writes
/// and accumulates bytes from each overlapped completion.
#[test]
fn writer_accumulates_across_implicit_flushes() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("writer_partial.bin");
    let payload = make_payload(payload_size());

    let config = IocpConfig {
        buffer_size: 8 * 1024,
        concurrent_ops: 1,
        unbuffered: false,
        write_through: false,
    };

    {
        let mut writer = IocpWriter::create(&path, &config).expect("create IocpWriter");

        // Sub-buffer slab so each `write_all` triggers at least one
        // implicit `flush_buffer` once the internal buffer fills.
        let slab = 3 * 1024;
        for chunk in payload.chunks(slab) {
            writer.write_all(chunk).expect("write_all under pressure");
        }
        writer.flush().expect("final flush");
        assert_eq!(
            writer.bytes_written() as usize,
            payload.len(),
            "IocpWriter::bytes_written must reflect every byte handed in"
        );
    }

    let on_disk = std::fs::read(&path).expect("read back writer output");
    assert_eq!(
        on_disk.len(),
        payload.len(),
        "on-disk size mismatch after IocpWriter drop"
    );
    assert!(
        on_disk == payload,
        "on-disk bytes diverge from input - implicit flush lost or reordered data"
    );
}

/// Exercises the chunk-boundary case: a single caller-side write spans
/// many internal buffer chunks, modelling the scenario where one logical
/// write reports a partial completion that the writer must split into
/// follow-up submissions. The expected outcome is byte-for-byte identity.
#[test]
fn writer_splits_single_write_across_many_chunks() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("writer_split.bin");

    // Pick a payload that is not a multiple of the buffer size so the
    // final partial chunk also exercises the trailing-write path.
    let payload = make_payload(1_500_001);

    let config = IocpConfig {
        buffer_size: 4 * 1024,
        concurrent_ops: 1,
        unbuffered: false,
        write_through: false,
    };

    {
        let mut writer = IocpWriter::create(&path, &config).expect("create IocpWriter");
        // One call - the writer is forced to split internally.
        writer
            .write_all(&payload)
            .expect("single write_all across many chunks");
        writer.flush().expect("flush");
        assert_eq!(writer.bytes_written() as usize, payload.len());
    }

    let on_disk = std::fs::read(&path).expect("read back");
    assert_eq!(on_disk.len(), payload.len());
    assert!(
        on_disk == payload,
        "single-write split across internal chunks corrupted the output"
    );
}

/// Error path: writing to an [`IocpDiskBatch`] with no active file returns
/// `InvalidInput`. This is the user-visible signature of "broken handle" at
/// the API layer - there is no overlapped handle to submit against.
#[test]
fn disk_batch_write_without_active_file_returns_invalid_input() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).expect("create batch");
    let err = batch
        .write_data(b"no active file")
        .expect_err("write_data must fail without an active file");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

/// Error path: `begin_file` against a read-only handle must fail because
/// `ReOpenFile` cannot upgrade the access mask to `FILE_GENERIC_WRITE`.
/// This proves the writer surfaces handle-broken errors rather than
/// silently swallowing them and reporting bogus success later.
#[test]
fn disk_batch_begin_file_with_readonly_handle_errors() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("readonly_handle.bin");
    std::fs::write(&path, b"pre-existing").expect("seed file");

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).expect("create batch");
    let readonly = open_readonly(&path);

    let result = batch.begin_file(readonly);
    assert!(
        result.is_err(),
        "begin_file must propagate ReOpenFile failure for a read-only handle"
    );
}

/// Error path: committing without an active file returns `InvalidInput`.
/// Pairs with the write-side check above to cover both ends of the
/// "broken handle" surface exposed by the batched writer.
#[test]
fn disk_batch_commit_without_active_file_returns_invalid_input() {
    if !is_iocp_available() {
        eprintln!("skipping IOCP partial-write test: IOCP unavailable");
        return;
    }

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).expect("create batch");
    let err = batch
        .commit_file(false)
        .expect_err("commit_file must fail without an active file");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}
