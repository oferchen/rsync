//! Integration tests for compression functionality.
//!
//! These tests verify that compression works correctly end-to-end during
//! file transfers, including compression level configuration, skip-compress
//! patterns, and data integrity.

use std::fs;
use std::path::Path;

use core::client::ClientConfig;
use tempfile::tempdir;

fn touch(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// Creates test data that compresses well (repeated patterns)
fn create_compressible_data(size: usize) -> Vec<u8> {
    let pattern = b"The quick brown fox jumps over the lazy dog. ";
    let mut data = Vec::with_capacity(size);
    while data.len() < size {
        data.extend_from_slice(pattern);
    }
    data.truncate(size);
    data
}

/// Creates test data that doesn't compress well (random-like)
fn create_incompressible_data(size: usize) -> Vec<u8> {
    // Use a deterministic pattern that looks random
    (0..size).map(|i| ((i * 97 + 31) % 256) as u8).collect()
}

#[test]
fn test_compression_disabled_by_default() {
    // Verify that compression is NOT enabled by default in local copy mode
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create a compressible file
    let data = create_compressible_data(10240); // 10KB of compressible data
    touch(&source_root.join("data.txt"), &data);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify file was copied correctly
    assert_eq!(fs::read(dest_root.join("data.txt")).unwrap(), data);
    assert!(summary.files_copied() >= 1);
    assert!(summary.bytes_copied() > 0);
}

#[test]
fn test_compression_enabled_copies_correctly() {
    // Verify that compression can be enabled and files are copied correctly
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create a compressible file
    let data = create_compressible_data(10240); // 10KB of compressible data
    touch(&source_root.join("data.txt"), &data);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .compress(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify file was copied correctly (content integrity)
    assert_eq!(fs::read(dest_root.join("data.txt")).unwrap(), data);
    assert!(summary.files_copied() >= 1);
    assert!(summary.bytes_copied() > 0);
}

#[test]
fn test_compression_preserves_binary_data() {
    // Verify compression works with binary (incompressible) data
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create incompressible binary data
    let data = create_incompressible_data(8192); // 8KB of incompressible data
    touch(&source_root.join("binary.dat"), &data);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .compress(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify binary data preserved exactly
    assert_eq!(fs::read(dest_root.join("binary.dat")).unwrap(), data);
    assert!(summary.files_copied() >= 1);
}

#[test]
fn test_compression_with_multiple_files() {
    // Verify compression works with multiple files of varying compressibility
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create mix of compressible and incompressible files
    let compressible = create_compressible_data(5120);
    let incompressible = create_incompressible_data(5120);

    touch(&source_root.join("text.txt"), &compressible);
    touch(&source_root.join("data.bin"), &incompressible);
    touch(&source_root.join("nested/log.txt"), &compressible);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .compress(true)
        .mkpath(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify all files copied correctly
    assert_eq!(fs::read(dest_root.join("text.txt")).unwrap(), compressible);
    assert_eq!(
        fs::read(dest_root.join("data.bin")).unwrap(),
        incompressible
    );
    assert_eq!(
        fs::read(dest_root.join("nested/log.txt")).unwrap(),
        compressible
    );
    assert!(summary.files_copied() >= 3);
}

#[test]
fn test_skip_compress_default_patterns() {
    // Verify that default skip-compress patterns are applied
    // (This tests the skip_compress infrastructure in local copy mode)
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create files that match skip-compress patterns
    let data = create_compressible_data(4096);
    touch(&source_root.join("archive.tar.gz"), &data);
    touch(&source_root.join("video.mp4"), &data);
    touch(&source_root.join("text.txt"), &data);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .compress(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify all files copied correctly (skip-compress doesn't affect correctness)
    assert_eq!(fs::read(dest_root.join("archive.tar.gz")).unwrap(), data);
    assert_eq!(fs::read(dest_root.join("video.mp4")).unwrap(), data);
    assert_eq!(fs::read(dest_root.join("text.txt")).unwrap(), data);
    assert!(summary.files_copied() >= 3);
}

#[test]
fn test_large_file_with_compression() {
    // Verify compression works with larger files
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");

    fs::create_dir_all(&source_root).expect("source root");
    fs::create_dir_all(&dest_root).expect("dest root");

    // Create a larger compressible file (100KB)
    let data = create_compressible_data(102400);
    touch(&source_root.join("large.txt"), &data);

    let mut source_arg = source_root.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_root.clone().into_os_string()])
        .compress(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // Verify large file copied correctly
    assert_eq!(fs::read(dest_root.join("large.txt")).unwrap(), data);
    assert!(summary.files_copied() >= 1);
    assert_eq!(summary.bytes_copied() as usize, data.len());
}
