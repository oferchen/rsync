//! Submission-helper tests: `submit_read_fixed_batch` and
//! `submit_write_fixed_batch` round-trips, short-read handling at EOF,
//! and small-file (one-chunk) coverage.

use std::os::unix::io::AsRawFd;

use io_uring::IoUring as RawIoUring;

use super::super::registry::{RegisteredBufferGroup, RegisteredBufferSlot};
use super::super::submit::{
    RegisteredBufferSlotInfo, submit_read_fixed_batch, submit_write_fixed_batch,
};
use super::{try_group, try_ring};

/// Checks out every available slot from `group` and returns both the
/// live slot handles and the `RegisteredBufferSlotInfo` views the
/// batch helpers consume. The slot handles MUST outlive the infos.
fn checkout_all(
    group: &RegisteredBufferGroup,
    count: usize,
) -> (Vec<RegisteredBufferSlot<'_>>, Vec<RegisteredBufferSlotInfo>) {
    let mut checked_out: Vec<RegisteredBufferSlot<'_>> =
        (0..count).filter_map(|_| group.checkout()).collect();
    let infos: Vec<RegisteredBufferSlotInfo> = checked_out
        .iter_mut()
        .map(|s| RegisteredBufferSlotInfo {
            ptr: s.as_mut_ptr(),
            buf_index: s.buf_index(),
            buffer_size: s.buffer_size(),
        })
        .collect();
    (checked_out, infos)
}

#[test]
fn read_fixed_write_fixed_roundtrip() {
    let Some(ring) = try_ring(64) else { return };
    let Some(group) = try_group(&ring, 4096, 4) else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixed_roundtrip.bin");

    // Generate test data larger than one buffer.
    let test_data: Vec<u8> = (0..12000u32).map(|i| (i % 256) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    let (checked_out, slot_infos) = checkout_all(&group, 4);

    // Read the file using ReadFixed.
    let file = std::fs::File::open(&path).unwrap();
    let fd = io_uring::types::Fd(file.as_raw_fd());

    let mut read_buf = vec![0u8; test_data.len()];
    let mut ring_rw: RawIoUring = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_read, test_data.len());
    assert_eq!(read_buf, test_data);

    // Write using WriteFixed to a new file.
    let write_path = dir.path().join("fixed_write_out.bin");
    let write_file = std::fs::File::create(&write_path).unwrap();
    let write_fd = io_uring::types::Fd(write_file.as_raw_fd());

    let bytes_written = submit_write_fixed_batch(
        &mut ring_rw,
        write_fd,
        &test_data,
        0,
        &slot_infos,
        super::super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_written, test_data.len());
    drop(write_file); // Flush.

    let written_data = std::fs::read(&write_path).unwrap();
    assert_eq!(written_data, test_data);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}

/// Reads with an output buffer larger than the file to trigger a natural
/// short read (EOF before buffer is full). Before the fix, the function
/// would advance past unread bytes, returning `total` even though the
/// file was smaller - silently zero-filling the tail.
#[test]
fn read_fixed_batch_short_read_at_eof() {
    let Some(ring) = try_ring(64) else { return };
    let Some(group) = try_group(&ring, 4096, 4) else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short_read.bin");

    // File is 5000 bytes but we ask to read 16384 (4 * 4096).
    let test_data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    let (checked_out, slot_infos) = checkout_all(&group, 4);

    let file = std::fs::File::open(&path).unwrap();
    let fd = io_uring::types::Fd(file.as_raw_fd());

    // Request more bytes than the file contains.
    let request_size = 4 * 4096;
    let mut read_buf = vec![0xFFu8; request_size];
    let mut ring_rw: RawIoUring = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    // Must return exactly the file size, not the request size.
    assert_eq!(bytes_read, test_data.len());
    assert_eq!(&read_buf[..bytes_read], &test_data[..]);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}

/// Reads a file that is smaller than a single registered buffer chunk.
/// The first SQE returns a short read (file size < chunk size), and the
/// function must report only the actual bytes read.
#[test]
fn read_fixed_batch_file_smaller_than_chunk() {
    let Some(ring) = try_ring(64) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.bin");

    let test_data = b"small file content";
    std::fs::write(&path, test_data).unwrap();

    let (checked_out, slot_infos) = checkout_all(&group, 2);

    let file = std::fs::File::open(&path).unwrap();
    let fd = io_uring::types::Fd(file.as_raw_fd());

    // Request 8192 bytes (2 chunks) but file is only 18 bytes.
    let mut read_buf = vec![0xFFu8; 8192];
    let mut ring_rw: RawIoUring = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_read, test_data.len());
    assert_eq!(&read_buf[..bytes_read], &test_data[..]);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}
