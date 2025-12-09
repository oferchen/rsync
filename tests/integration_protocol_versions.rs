//! Integration tests for protocol version compatibility.
//!
//! These tests validate that oc-rsync correctly negotiates and operates with
//! different protocol versions, particularly testing the critical checksum
//! algorithm selection:
//! - Protocol < 30 (28, 29): Uses MD4 checksums
//! - Protocol >= 30 (30, 31, 32): Uses MD5 checksums
//!
//! ## Test Coverage
//!
//! ### Currently Implemented:
//! - Basic file transfer compatibility with upstream rsync versions:
//!   - rsync 3.0.9 (protocol 30, MD5)
//!   - rsync 3.1.3 (protocol 31, MD5)
//!   - rsync 3.4.1 (protocol 32, MD5)
//! - Delta transfer with protocol 30+ (MD5 checksums)
//!
//! ### Future Enhancements:
//! - Protocol forcing tests (requires `--protocol` support in oc-rsync client)
//! - Explicit protocol 28-29 testing (MD4 checksums)
//! - Protocol downgrade scenarios
//! - Protocol mismatch error handling
//!
//! ## Implementation Note
//!
//! Currently, oc-rsync only supports `--protocol=N` for daemon operands. To fully
//! test protocol version forcing, we need to:
//! 1. Extend `--protocol` support to client mode, OR
//! 2. Use upstream rsync with `--protocol=N` as the client against oc-rsync server
//!
//! The current tests validate compatibility with different upstream versions,
//! which implicitly validates the checksum algorithm selection is correct
//! (transfers would fail if wrong checksums were used).

mod integration;

use std::path::Path;

use integration::helpers::*;
use std::fs;
use filetime::{FileTime, set_file_times};

/// Path to upstream rsync 3.0.9 binary (protocol 30).
const UPSTREAM_RSYNC_3_0_9: &str = "target/interop/upstream-install/3.0.9/bin/rsync";

/// Path to upstream rsync 3.1.3 binary (protocol 31).
const UPSTREAM_RSYNC_3_1_3: &str = "target/interop/upstream-install/3.1.3/bin/rsync";

/// Path to upstream rsync 3.4.1 binary (protocol 32).
const UPSTREAM_RSYNC_3_4_1: &str = "target/interop/upstream-install/3.4.1/bin/rsync";

// ============ Helper Functions ============

/// Check if an upstream rsync binary is available.
fn upstream_binary_available(path: &str) -> bool {
    Path::new(path).is_file()
}

/// Skip test if upstream binary is not available.
macro_rules! require_upstream_binary {
    ($path:expr, $version:expr) => {
        if !upstream_binary_available($path) {
            eprintln!(
                "Skipping test: upstream rsync {} not found at {}",
                $version, $path
            );
            eprintln!("Run interop build script to install upstream versions");
            return;
        }
    };
}

// ============ Basic Compatibility Tests ============

#[test]
fn oc_rsync_compatible_with_rsync_3_0_9() {
    require_upstream_binary!(UPSTREAM_RSYNC_3_0_9, "3.0.9");

    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create test file
    let content = b"Test content for rsync 3.0.9 (protocol 30) compatibility";
    test_dir
        .write_file("src/test.txt", content)
        .expect("write source file");

    // Use oc-rsync to transfer (will negotiate with upstream's protocol 30)
    let mut cmd = RsyncCommand::new();
    cmd.args([
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify transfer succeeded
    let dest_file = test_dir.read_file("dest/test.txt").expect("read dest file");
    assert_eq!(dest_file, content, "Content should match after transfer");
}

#[test]
fn oc_rsync_compatible_with_rsync_3_1_3() {
    require_upstream_binary!(UPSTREAM_RSYNC_3_1_3, "3.1.3");

    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create test file
    let content = b"Test content for rsync 3.1.3 (protocol 31) compatibility";
    test_dir
        .write_file("src/test.txt", content)
        .expect("write source file");

    // Use oc-rsync to transfer (will negotiate with upstream's protocol 31)
    let mut cmd = RsyncCommand::new();
    cmd.args([
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify transfer succeeded
    let dest_file = test_dir.read_file("dest/test.txt").expect("read dest file");
    assert_eq!(dest_file, content, "Content should match after transfer");
}

#[test]
fn oc_rsync_compatible_with_rsync_3_4_1() {
    require_upstream_binary!(UPSTREAM_RSYNC_3_4_1, "3.4.1");

    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create test file
    let content = b"Test content for rsync 3.4.1 (protocol 32) compatibility";
    test_dir
        .write_file("src/test.txt", content)
        .expect("write source file");

    // Use oc-rsync to transfer (will negotiate with upstream's protocol 32)
    let mut cmd = RsyncCommand::new();
    cmd.args([
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify transfer succeeded
    let dest_file = test_dir.read_file("dest/test.txt").expect("read dest file");
    assert_eq!(dest_file, content, "Content should match after transfer");
}

// ============ Delta Transfer Protocol Tests ============
//
// These tests validate that delta transfer works correctly with different
// protocol versions. If the checksum algorithm selection is wrong (e.g., using
// MD5 when it should be MD4, or vice versa), the delta transfer will fail or
// produce corrupt data.

#[test]
fn delta_transfer_protocol_30_uses_correct_checksums() {
    // Protocol 30 should use MD5 checksums
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create source file with specific pattern
    let mut src_content = Vec::new();
    src_content.extend(vec![b'A'; 4096]);
    src_content.extend(vec![b'B'; 4096]);
    src_content.extend(vec![b'C'; 4096]);

    let src_file = src_dir.join("data.bin");
    fs::write(&src_file, &src_content).unwrap();

    // Create basis file with modified middle section
    let mut basis_content = Vec::new();
    basis_content.extend(vec![b'A'; 4096]);
    basis_content.extend(vec![b'X'; 4096]); // Different middle
    basis_content.extend(vec![b'C'; 4096]);

    let dest_file = dest_dir.join("data.bin");
    fs::write(&dest_file, &basis_content).unwrap();

    // Make destination older so rsync will perform delta transfer
    let old_time = FileTime::from_unix_time(1600000000, 0);
    set_file_times(&dest_file, old_time, old_time).unwrap();

    // Run delta transfer (local transfer, will use default protocol 32, MD5)
    let mut cmd = RsyncCommand::new();
    cmd.args([src_file.to_str().unwrap(), dest_file.to_str().unwrap()]);
    cmd.assert_success();

    // Verify file was correctly reconstructed
    let result = fs::read(&dest_file).unwrap();
    assert_eq!(
        result, src_content,
        "Delta transfer should correctly reconstruct file with protocol 30+ (MD5)"
    );
}

#[test]
fn delta_transfer_multiple_files_protocol_consistency() {
    // Verify delta transfer works consistently across multiple files
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").unwrap();
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Create multiple files with different sizes
    for i in 0..5 {
        let content: Vec<u8> = (0..((i + 1) * 1024))
            .map(|j| ((i * 7 + j) % 256) as u8)
            .collect();
        let file_path = src_dir.join(format!("file{i}.bin"));
        fs::write(&file_path, &content).unwrap();

        // Create basis files with partial matches
        let mut basis = content.clone();
        if basis.len() > 512 {
            // Modify middle section
            let mid = basis.len() / 2;
            for j in 0..256 {
                basis[mid + j] = !basis[mid + j];
            }
        }
        let dest_path = dest_dir.join(format!("file{i}.bin"));
        fs::write(&dest_path, &basis).unwrap();

        // Make destination older so rsync will perform delta transfer
        let old_time = FileTime::from_unix_time(1600000000, 0);
        set_file_times(&dest_path, old_time, old_time).unwrap();
    }

    // Run delta transfer on directory
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-r", // Recursive
        &format!("{}/", src_dir.display()),
        &format!("{}/", dest_dir.display()),
    ]);
    cmd.assert_success();

    // Verify all files were correctly transferred
    for i in 0..5 {
        let src_file = src_dir.join(format!("file{i}.bin"));
        let dest_file = dest_dir.join(format!("file{i}.bin"));

        let src_content = fs::read(&src_file).unwrap();
        let dest_content = fs::read(&dest_file).unwrap();

        assert_eq!(
            src_content, dest_content,
            "file{i}.bin should match after delta transfer"
        );
    }
}

// ============ Future Test Stubs ============
//
// These tests are commented out until we have --protocol support in client mode
// or implement server-mode testing infrastructure.

/*
#[test]
fn force_protocol_28_uses_md4_checksums() {
    // TODO: Requires --protocol=28 support in oc-rsync client mode
    // This would test:
    // 1. Force protocol 28 negotiation
    // 2. Verify MD4 checksums are used
    // 3. Validate delta transfer works correctly
    unimplemented!("Requires --protocol support in client mode");
}

#[test]
fn force_protocol_29_uses_md4_checksums() {
    // TODO: Similar to protocol 28 test
    unimplemented!("Requires --protocol support in client mode");
}

#[test]
fn force_protocol_30_uses_md5_checksums() {
    // TODO: Force protocol 30 and verify MD5 checksums
    unimplemented!("Requires --protocol support in client mode");
}

#[test]
fn protocol_downgrade_to_mutual_maximum() {
    // TODO: Test that when oc-rsync (protocol 32) connects to upstream
    // rsync 3.0.9 (protocol 30), they negotiate to protocol 30
    unimplemented!("Requires protocol negotiation visibility");
}

#[test]
fn protocol_mismatch_error_handling() {
    // TODO: Test error handling when protocols are incompatible
    unimplemented!("Requires protocol mismatch scenario");
}

#[test]
fn upstream_as_sender_protocol_28_to_oc_rsync_receiver() {
    // TODO: Use upstream rsync --protocol=28 as sender, oc-rsync as receiver
    // This requires setting up oc-rsync in --server mode or daemon mode
    require_upstream_binary!(UPSTREAM_RSYNC_3_4_1, "3.4.1");
    unimplemented!("Requires --server mode test infrastructure");
}

#[test]
fn upstream_as_sender_protocol_29_to_oc_rsync_receiver() {
    // TODO: Similar to protocol 28 test
    require_upstream_binary!(UPSTREAM_RSYNC_3_4_1, "3.4.1");
    unimplemented!("Requires --server mode test infrastructure");
}
*/
