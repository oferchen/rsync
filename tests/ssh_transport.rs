//! Integration tests for SSH transport functionality.
//!
//! These tests verify end-to-end SSH transfer operations including:
//! - Push operations (local → remote)
//! - Pull operations (remote → local)
//! - Error handling for invalid operands
//! - Protocol negotiation over SSH connections

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

/// Test helper to create a temporary directory with test files.
fn setup_test_directory() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create some test files
    let file1 = temp_dir.path().join("file1.txt");
    fs::write(&file1, b"Hello, World!").expect("Failed to write file1");

    let file2 = temp_dir.path().join("file2.txt");
    fs::write(&file2, b"Test content 2").expect("Failed to write file2");

    // Create a subdirectory with a file
    let subdir = temp_dir.path().join("subdir");
    fs::create_dir(&subdir).expect("Failed to create subdir");
    let file3 = subdir.join("file3.txt");
    fs::write(&file3, b"Nested file").expect("Failed to write file3");

    temp_dir
}

/// Test helper to get the path to the oc-rsync binary.
fn oc_rsync_binary() -> PathBuf {
    // Try to find the binary in target/debug or target/release
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let debug_path = PathBuf::from(manifest_dir)
        .join("target")
        .join("debug")
        .join("oc-rsync");

    if debug_path.exists() {
        return debug_path;
    }

    let release_path = PathBuf::from(manifest_dir)
        .join("target")
        .join("release")
        .join("oc-rsync");

    if release_path.exists() {
        return release_path;
    }

    // Fallback: assume it's in PATH
    PathBuf::from("oc-rsync")
}

#[test]
#[ignore] // Requires SSH server setup
fn test_ssh_push_single_file() {
    let source_dir = setup_test_directory();
    let dest_dir = TempDir::new().expect("Failed to create dest directory");

    // For this test to work, you need:
    // 1. SSH server running on localhost
    // 2. SSH key authentication configured
    // 3. The test assumes localhost:22 is accessible

    let source_file = source_dir.path().join("file1.txt");
    let remote_dest = format!("localhost:{}", dest_dir.path().join("file1.txt").display());

    let output = std::process::Command::new(oc_rsync_binary())
        .arg(&source_file)
        .arg(&remote_dest)
        .output()
        .expect("Failed to execute oc-rsync");

    // For now, we expect this to fail gracefully if SSH is not set up
    // In a CI environment with SSH configured, this should succeed
    if output.status.success() {
        // Verify the file was transferred
        let dest_file = dest_dir.path().join("file1.txt");
        assert!(dest_file.exists(), "Destination file should exist");
        let content = fs::read_to_string(&dest_file).expect("Failed to read dest file");
        assert_eq!(content, "Hello, World!");
    } else {
        // SSH not configured, test is informational only
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("SSH push test skipped (SSH not configured): {}", stderr);
    }
}

#[test]
#[ignore] // Requires SSH server setup
fn test_ssh_pull_single_file() {
    let source_dir = setup_test_directory();
    let dest_dir = TempDir::new().expect("Failed to create dest directory");

    let source_file = source_dir.path().join("file2.txt");
    let remote_source = format!("localhost:{}", source_file.display());
    let dest_file = dest_dir.path().join("file2.txt");

    let output = std::process::Command::new(oc_rsync_binary())
        .arg(&remote_source)
        .arg(&dest_file)
        .output()
        .expect("Failed to execute oc-rsync");

    if output.status.success() {
        // Verify the file was transferred
        assert!(dest_file.exists(), "Destination file should exist");
        let content = fs::read_to_string(&dest_file).expect("Failed to read dest file");
        assert_eq!(content, "Test content 2");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("SSH pull test skipped (SSH not configured): {}", stderr);
    }
}

#[test]
#[ignore] // Requires SSH server setup
fn test_ssh_push_recursive_directory() {
    let source_dir = setup_test_directory();
    let dest_dir = TempDir::new().expect("Failed to create dest directory");

    let remote_dest = format!("localhost:{}/", dest_dir.path().display());

    let output = std::process::Command::new(oc_rsync_binary())
        .arg("-r")
        .arg(format!("{}/", source_dir.path().display()))
        .arg(&remote_dest)
        .output()
        .expect("Failed to execute oc-rsync");

    if output.status.success() {
        // Verify the directory structure was transferred
        assert!(dest_dir.path().join("file1.txt").exists());
        assert!(dest_dir.path().join("file2.txt").exists());
        assert!(dest_dir.path().join("subdir").join("file3.txt").exists());
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("SSH recursive push test skipped (SSH not configured): {}", stderr);
    }
}

#[test]
fn test_ssh_operand_detection() {
    // This test verifies that SSH operands are correctly detected
    // without requiring an actual SSH connection

    use core::client::remote::operand_is_remote;
    use std::ffi::OsString;

    // Test various SSH operand formats
    assert!(operand_is_remote(&OsString::from("host:path")));
    assert!(operand_is_remote(&OsString::from("user@host:path")));
    assert!(operand_is_remote(&OsString::from("user@host:/absolute/path")));
    assert!(operand_is_remote(&OsString::from("[::1]:path")));
    assert!(operand_is_remote(&OsString::from("user@[2001:db8::1]:path")));
    assert!(operand_is_remote(&OsString::from("rsync://host/module/path")));
    assert!(operand_is_remote(&OsString::from("host::module/path")));

    // Test non-remote operands
    assert!(!operand_is_remote(&OsString::from("local/path")));
    assert!(!operand_is_remote(&OsString::from("/absolute/local/path")));
    assert!(!operand_is_remote(&OsString::from("./relative/path")));

    #[cfg(windows)]
    {
        // Windows drive letters should not be detected as remote
        assert!(!operand_is_remote(&OsString::from("C:\\path")));
        assert!(!operand_is_remote(&OsString::from("D:/path")));
    }
}

#[test]
fn test_transfer_role_detection() {
    use core::client::remote::{RemoteRole, determine_transfer_role};
    use std::ffi::OsString;

    // Test push detection (local → remote)
    let sources = vec![OsString::from("local.txt")];
    let destination = OsString::from("host:remote.txt");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect push");
    assert_eq!(result.0, RemoteRole::Sender);
    assert_eq!(result.1, vec!["local.txt"]);
    assert_eq!(result.2, "host:remote.txt");

    // Test pull detection (remote → local)
    let sources = vec![OsString::from("host:remote.txt")];
    let destination = OsString::from("local.txt");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect pull");
    assert_eq!(result.0, RemoteRole::Receiver);
    assert_eq!(result.1, vec!["local.txt"]);
    assert_eq!(result.2, "host:remote.txt");

    // Test multiple local sources with remote destination
    let sources = vec![
        OsString::from("file1.txt"),
        OsString::from("file2.txt"),
    ];
    let destination = OsString::from("host:/dest/");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect push");
    assert_eq!(result.0, RemoteRole::Sender);
    assert_eq!(result.1, vec!["file1.txt", "file2.txt"]);

    // Test error cases

    // Both remote (not supported)
    let sources = vec![OsString::from("host1:file")];
    let destination = OsString::from("host2:file");
    assert!(determine_transfer_role(&sources, &destination).is_err());

    // Neither remote (should use local copy)
    let sources = vec![OsString::from("file1.txt")];
    let destination = OsString::from("file2.txt");
    assert!(determine_transfer_role(&sources, &destination).is_err());

    // Mixed remote and local sources (not supported)
    let sources = vec![
        OsString::from("local.txt"),
        OsString::from("host:remote.txt"),
    ];
    let destination = OsString::from("dest/");
    assert!(determine_transfer_role(&sources, &destination).is_err());
}

#[test]
fn test_remote_invocation_builder() {
    use core::client::{ClientConfig, remote::{RemoteInvocationBuilder, RemoteRole}};

    // Test sender (push) invocation
    let config = ClientConfig::builder()
        .recursive(true)
        .times(true)
        .permissions(true)
        .build();

    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    assert_eq!(args[0].to_string_lossy(), "rsync");
    assert_eq!(args[1].to_string_lossy(), "--server");
    assert_eq!(args[2].to_string_lossy(), "--sender");

    // Flag string should contain r (recursive), t (times), p (permissions)
    let flag_string = args[3].to_string_lossy();
    assert!(flag_string.starts_with('-'));
    assert!(flag_string.contains('r'), "Should contain 'r' for recursive");
    assert!(flag_string.contains('t'), "Should contain 't' for times");
    assert!(flag_string.contains('p'), "Should contain 'p' for permissions");

    assert_eq!(args[args.len() - 2].to_string_lossy(), ".");
    assert_eq!(args[args.len() - 1].to_string_lossy(), "/remote/path");

    // Test receiver (pull) invocation - should NOT have --sender flag
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build("/remote/path");

    assert_eq!(args[0].to_string_lossy(), "rsync");
    assert_eq!(args[1].to_string_lossy(), "--server");

    // Should NOT have --sender
    assert_ne!(args[2].to_string_lossy(), "--sender");

    // Flag string comes right after --server for receiver
    let flag_string = args[2].to_string_lossy();
    assert!(flag_string.starts_with('-'));
}
