use super::{
    IocpConfig, IocpDiskBatch, bounce_copies_avoided, reset_bounce_copies_avoided_for_test,
};
use std::fs::{self, File};
use std::io::{self, Write};
use tempfile::tempdir;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

fn to_wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn open_writable(path: &std::path::Path) -> File {
    let wide = to_wide(path);
    // SAFETY: Standard Win32 open: zero-terminated wide string, generic
    // write, create-always. The handle permits shared read/write/delete
    // so that `ReOpenFile` (called from `begin_file`) can acquire a
    // second overlapped write handle without ERROR_SHARING_VIOLATION,
    // and so the enclosing tempdir can be cleaned up. The returned
    // handle is wrapped into a std::fs::File via FromRawHandle so Drop
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
    // SAFETY: `handle` is a fresh, exclusively-owned handle.
    #[allow(unsafe_code)]
    unsafe {
        use std::os::windows::io::FromRawHandle;
        File::from_raw_handle(handle as *mut std::ffi::c_void)
    }
}

#[test]
fn try_new_returns_some_on_windows() {
    let config = IocpConfig::default();
    let batch = IocpDiskBatch::try_new(&config);
    assert!(
        batch.is_some(),
        "IOCP must be available on every supported Windows host"
    );
}

#[test]
fn write_without_active_file_errors() {
    let config = IocpConfig::default();
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    let result = batch.write_data(b"hello");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn commit_without_active_file_errors() {
    let config = IocpConfig::default();
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    let result = batch.commit_file(false);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn bytes_written_accessors_default_to_zero() {
    let config = IocpConfig::default();
    let batch = IocpDiskBatch::new(&config).unwrap();
    assert_eq!(batch.bytes_written(), 0);
    assert_eq!(batch.bytes_written_with_pending(), 0);
}

#[test]
fn single_file_write_and_commit() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("single.bin");
    let file = open_writable(&path);

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
    batch.begin_file(file).unwrap();

    let payload = b"hello iocp disk batch";
    batch.write_data(payload).unwrap();
    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written as usize, payload.len());

    let content = fs::read(&path).unwrap();
    assert_eq!(content, payload);
}

#[test]
fn multi_file_sequential_writes() {
    let dir = tempdir().unwrap();
    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

    let test_data: Vec<(&str, Vec<u8>)> = vec![
        ("file_a.bin", vec![0xAA; 1024]),
        ("file_b.bin", vec![0xBB; 4096]),
        ("file_c.bin", vec![0xCC; 128]),
    ];

    for (name, data) in &test_data {
        let path = dir.path().join(name);
        let file = open_writable(&path);
        batch.begin_file(file).unwrap();
        batch.write_data(data).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, data.len());
    }

    for (name, data) in &test_data {
        let content = fs::read(dir.path().join(name)).unwrap();
        assert_eq!(content, *data, "content mismatch for {name}");
    }
}

#[test]
fn large_write_exceeds_buffer_drains_via_completion_port() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("large.bin");
    let file = open_writable(&path);

    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 4,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    let data: Vec<u8> = (0..32_768).map(|i| (i % 256) as u8).collect();
    batch.write_data(&data).unwrap();
    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written as usize, data.len());

    let content = fs::read(&path).unwrap();
    assert_eq!(content, data);
}

#[test]
fn commit_with_fsync_calls_flush_file_buffers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("fsync.bin");
    let file = open_writable(&path);

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
    batch.begin_file(file).unwrap();
    batch.write_data(b"durable").unwrap();
    let (_returned, written) = batch.commit_file(true).unwrap();
    assert_eq!(written, 7);

    let content = fs::read(&path).unwrap();
    assert_eq!(content, b"durable");
}

#[test]
fn begin_file_flushes_previous() {
    let dir = tempdir().unwrap();
    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

    let path1 = dir.path().join("first.bin");
    let file1 = open_writable(&path1);
    batch.begin_file(file1).unwrap();
    batch.write_data(b"first file data").unwrap();

    let path2 = dir.path().join("second.bin");
    let file2 = open_writable(&path2);
    batch.begin_file(file2).unwrap();

    // First file should be on disk after the rotation flush.
    let content1 = fs::read(&path1).unwrap();
    assert_eq!(content1, b"first file data");

    batch.write_data(b"second").unwrap();
    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written, 6);
}

#[test]
fn drop_flushes_pending_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("drop_flush.bin");

    {
        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
        let file = open_writable(&path);
        batch.begin_file(file).unwrap();
        batch.write_data(b"drop test").unwrap();
        // No explicit commit - rely on Drop.
    }

    let content = fs::read(&path).unwrap();
    assert_eq!(content, b"drop test");
}

#[test]
fn write_trait_implementation_round_trips() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("write_trait.bin");
    let file = open_writable(&path);

    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
    batch.begin_file(file).unwrap();

    let n = Write::write(&mut batch, b"hello ").unwrap();
    assert_eq!(n, 6);
    Write::write_all(&mut batch, b"world").unwrap();
    Write::flush(&mut batch).unwrap();

    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written, 11);

    let content = fs::read(&path).unwrap();
    assert_eq!(content, b"hello world");
}

#[test]
fn batched_submission_submits_n_chunks() {
    // Pick buffer_size and concurrent_ops so the data triggers multiple
    // overlapped submissions per flush.
    let dir = tempdir().unwrap();
    let path = dir.path().join("batched.bin");
    let file = open_writable(&path);

    let config = IocpConfig {
        buffer_size: 1024,
        concurrent_ops: 8,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    // 16 chunks of 1 KB each = 16 KB total. With 8 in-flight, two drain
    // cycles are needed.
    let data: Vec<u8> = (0..16 * 1024).map(|i| (i & 0xFF) as u8).collect();
    batch.write_data(&data).unwrap();
    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written as usize, data.len());

    let content = fs::read(&path).unwrap();
    assert_eq!(content, data);
}

#[test]
fn error_propagates_when_reopen_overlapped_fails() {
    // Open with read-only access. begin_file calls ReOpenFile asking
    // for FILE_GENERIC_WRITE which the original handle was not opened
    // with, causing ReOpenFile to fail.
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, OPEN_EXISTING};

    let config = IocpConfig::default();
    let mut batch = IocpDiskBatch::new(&config).unwrap();

    let dir = tempdir().unwrap();
    let path = dir.path().join("readonly_target.bin");
    std::fs::write(&path, b"existing").unwrap();

    let wide = to_wide(&path);
    // SAFETY: standard Win32 open with read-only access.
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
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    // SAFETY: `handle` is freshly opened and exclusively owned here.
    #[allow(unsafe_code)]
    let file = unsafe {
        use std::os::windows::io::FromRawHandle;
        File::from_raw_handle(handle as *mut std::ffi::c_void)
    };

    let result = batch.begin_file(file);
    assert!(
        result.is_err(),
        "begin_file must surface ReOpenFile failure when the original handle lacks write access"
    );
}

#[test]
fn no_leaked_overlapped_handles_after_many_rotations() {
    // Round-trip many begin/commit cycles. If the overlapped handle were
    // leaked per file the process would eventually exhaust its handle
    // table; here we exercise the path 32 times and verify each file
    // lands intact.
    let dir = tempdir().unwrap();
    let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

    for i in 0..32 {
        let path = dir.path().join(format!("rotated_{i}.bin"));
        let file = open_writable(&path);
        batch.begin_file(file).unwrap();
        let payload = format!("rotation #{i}");
        batch.write_data(payload.as_bytes()).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, payload.len());

        let content = fs::read(&path).unwrap();
        assert_eq!(content, payload.as_bytes());
    }
}

#[test]
fn overlapped_handle_guard_closes_handle_on_drop() {
    // The RAII guard must close the reopened overlapped handle when it drops,
    // including on early-return/error paths that never reach commit_file. Drive
    // the guard directly: reopen a handle, capture its raw value, drop the
    // guard, and confirm the kernel no longer recognises the handle.
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::GetHandleInformation;

    let dir = tempdir().unwrap();
    let path = dir.path().join("guard_drop.bin");
    let file = open_writable(&path);
    let raw = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;

    let config = IocpConfig::default();
    let guard = super::writer::reopen_overlapped(raw, &config).unwrap();
    let reopened = guard.raw();

    // While the guard is alive the reopened handle is valid.
    let mut flags: u32 = 0;
    // SAFETY: `reopened` is a live handle owned by `guard`.
    #[allow(unsafe_code)]
    let ok_before = unsafe { GetHandleInformation(reopened, &mut flags) };
    assert_ne!(ok_before, 0, "reopened handle must be valid before drop");

    drop(guard);

    // After drop the handle value should no longer resolve. GetHandleInformation
    // returns 0 (with ERROR_INVALID_HANDLE) for a closed handle.
    // SAFETY: querying a (now closed) handle value is defined; the call only
    // reads kernel handle-table state and cannot dereference user memory.
    #[allow(unsafe_code)]
    let ok_after = unsafe { GetHandleInformation(reopened, &mut flags) };
    assert_eq!(
        ok_after, 0,
        "guard drop must close the reopened overlapped handle"
    );
}

#[test]
fn completion_ordering_independent_of_submission_order() {
    // Multiple in-flight writes may complete out of order. The drain
    // loop must reconcile each completion with its OVERLAPPED pointer
    // and produce the correct file contents regardless of order.
    let dir = tempdir().unwrap();
    let path = dir.path().join("ordering.bin");
    let file = open_writable(&path);

    let config = IocpConfig {
        buffer_size: 4096,
        concurrent_ops: 8,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    batch.begin_file(file).unwrap();

    // 8 distinct chunks of 4 KB each, each tagged with its index so we
    // can verify positional correctness regardless of completion order.
    let mut data = Vec::with_capacity(8 * 4096);
    for chunk_idx in 0..8u8 {
        data.extend(std::iter::repeat(chunk_idx).take(4096));
    }
    batch.write_data(&data).unwrap();
    let (_returned, written) = batch.commit_file(false).unwrap();
    assert_eq!(written as usize, data.len());

    let content = fs::read(&path).unwrap();
    assert_eq!(content, data);
}

#[test]
fn unbuffered_config_uses_page_aligned_buffer() {
    let config = IocpConfig {
        unbuffered: true,
        ..IocpConfig::default()
    };
    let batch = IocpDiskBatch::new(&config).unwrap();
    assert!(
        batch.buffer_is_page_aligned(),
        "unbuffered config must allocate a page-aligned accumulation buffer"
    );
}

#[test]
fn buffered_config_uses_vec_buffer() {
    let config = IocpConfig::default();
    let batch = IocpDiskBatch::new(&config).unwrap();
    assert!(
        !batch.buffer_is_page_aligned(),
        "buffered config must keep the standard heap buffer to avoid \
         paying the VirtualAlloc cost when the kernel will copy anyway"
    );
}

#[test]
fn aligned_write_path_increments_bounce_counter() {
    // Note: this exercises only the counter wiring - we cannot easily
    // verify a real ReOpenFile with FILE_FLAG_NO_BUFFERING here because
    // some tempfs volumes reject the flag. The page-aligned submission
    // path is the part that defeats the kernel bounce copy; the counter
    // increments on every aligned submission regardless of whether the
    // backing volume honours the no-buffering flag.
    reset_bounce_copies_avoided_for_test();
    let before = bounce_copies_avoided();
    assert_eq!(before, 0, "counter must start at zero after reset");

    let config = IocpConfig {
        unbuffered: false, // keep buffered so ReOpenFile succeeds on tempfs
        buffer_size: 64 * 1024,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).unwrap();
    let dir = tempdir().unwrap();
    let path = dir.path().join("aligned_counter.bin");
    let file = open_writable(&path);
    batch.begin_file(file).unwrap();

    // Drive a submission through the aligned path directly so we can
    // observe the counter without depending on filesystem support.
    let mut written = 0usize;
    super::writer::submit_write_batch(
        &batch.port,
        batch.current_file.as_ref().unwrap().overlapped_handle.raw(),
        &vec![0x42u8; 4096],
        0,
        4096,
        1,
        true,
        &mut written,
    )
    .unwrap();
    assert_eq!(written, 4096);
    assert!(
        bounce_copies_avoided() >= 1,
        "aligned submission must bump the bounce-copy counter"
    );
    let _ = batch.commit_file(false);
}
