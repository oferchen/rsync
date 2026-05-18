//! Tests for `try_vmsplice_to_file` and `SplicePipe::vmsplice_to_file` on Linux.

use super::super::super::*;
use std::io::{Read, Seek, SeekFrom};
use tempfile::NamedTempFile;

#[test]
fn test_vmsplice_small_buffer() {
    if !is_splice_available() {
        return;
    }

    let content = b"Testing vmsplice: buffer to file via pipe intermediary";
    let mut dest = NamedTempFile::new().unwrap();

    use std::os::fd::AsRawFd;
    let transferred = try_vmsplice_to_file(content, dest.as_file().as_raw_fd()).unwrap();

    assert_eq!(transferred, content.len());

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_vmsplice_empty_buffer() {
    if !is_splice_available() {
        return;
    }

    let mut dest = NamedTempFile::new().unwrap();

    use std::os::fd::AsRawFd;
    let transferred = try_vmsplice_to_file(&[], dest.as_file().as_raw_fd()).unwrap();

    assert_eq!(transferred, 0);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert!(file_content.is_empty());
}

#[test]
fn test_vmsplice_large_buffer() {
    if !is_splice_available() {
        return;
    }

    // 512KB - multiple splice chunks worth of data.
    let size = 512 * 1024;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    use std::os::fd::AsRawFd;
    let transferred = try_vmsplice_to_file(&content, dest.as_file().as_raw_fd()).unwrap();

    assert_eq!(transferred, size);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content.len(), content.len());
    assert_eq!(file_content, content);
}

#[test]
fn test_vmsplice_exact_chunk_boundary() {
    if !is_splice_available() {
        return;
    }

    // Exactly SPLICE_CHUNK_SIZE bytes.
    let size = super::super::super::SPLICE_CHUNK_SIZE;
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut dest = NamedTempFile::new().unwrap();

    use std::os::fd::AsRawFd;
    let transferred = try_vmsplice_to_file(&content, dest.as_file().as_raw_fd()).unwrap();

    assert_eq!(transferred, size);

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_vmsplice_via_splice_pipe() {
    if !is_splice_available() {
        return;
    }

    let content = b"Testing vmsplice through SplicePipe method";
    let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY).unwrap();
    let mut dest = NamedTempFile::new().unwrap();

    use std::os::fd::AsRawFd;
    let transferred = pipe
        .vmsplice_to_file(content, dest.as_file().as_raw_fd())
        .unwrap();

    assert_eq!(transferred, content.len());

    dest.seek(SeekFrom::Start(0)).unwrap();
    let mut file_content = Vec::new();
    dest.read_to_end(&mut file_content).unwrap();
    assert_eq!(file_content, content);
}

#[test]
fn test_vmsplice_invalid_fd_returns_error() {
    if !is_splice_available() {
        return;
    }

    let content = b"test data for invalid fd";
    let result = try_vmsplice_to_file(content, -1);
    assert!(result.is_err());
}
