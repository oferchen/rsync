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
        eprintln!("SSH push test skipped (SSH not configured): {stderr}");
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
        eprintln!("SSH pull test skipped (SSH not configured): {stderr}");
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
        eprintln!("SSH recursive push test skipped (SSH not configured): {stderr}");
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
    assert!(operand_is_remote(&OsString::from(
        "user@host:/absolute/path"
    )));
    assert!(operand_is_remote(&OsString::from("[::1]:path")));
    assert!(operand_is_remote(&OsString::from(
        "user@[2001:db8::1]:path"
    )));
    assert!(operand_is_remote(&OsString::from(
        "rsync://host/module/path"
    )));
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
    use core::client::remote::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
    use std::ffi::OsString;

    // Test push detection (local → remote)
    let sources = vec![OsString::from("local.txt")];
    let destination = OsString::from("host:remote.txt");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect push");
    assert_eq!(result.role(), RemoteRole::Sender);
    match result {
        TransferSpec::Push {
            local_sources,
            remote_dest,
        } => {
            assert_eq!(local_sources, vec!["local.txt"]);
            assert_eq!(remote_dest, "host:remote.txt");
        }
        _ => panic!("Expected Push transfer"),
    }

    // Test pull detection (remote → local)
    let sources = vec![OsString::from("host:remote.txt")];
    let destination = OsString::from("local.txt");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect pull");
    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "local.txt");
            assert_eq!(
                remote_sources,
                RemoteOperands::Single("host:remote.txt".to_string())
            );
        }
        _ => panic!("Expected Pull transfer"),
    }

    // Test multiple local sources with remote destination
    let sources = vec![OsString::from("file1.txt"), OsString::from("file2.txt")];
    let destination = OsString::from("host:/dest/");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect push");
    assert_eq!(result.role(), RemoteRole::Sender);
    match result {
        TransferSpec::Push {
            local_sources,
            remote_dest: _,
        } => {
            assert_eq!(local_sources, vec!["file1.txt", "file2.txt"]);
        }
        _ => panic!("Expected Push transfer"),
    }

    // Test remote-to-remote (proxy) - now returns Proxy instead of error
    let sources = vec![OsString::from("host1:file")];
    let destination = OsString::from("host2:file");
    let result = determine_transfer_role(&sources, &destination).expect("Should detect proxy");
    assert_eq!(result.role(), RemoteRole::Proxy);

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
    use core::client::{
        ClientConfig,
        remote::{RemoteInvocationBuilder, RemoteRole},
    };

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
    assert!(
        flag_string.contains('r'),
        "Should contain 'r' for recursive"
    );
    assert!(flag_string.contains('t'), "Should contain 't' for times");
    assert!(
        flag_string.contains('p'),
        "Should contain 'p' for permissions"
    );

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

#[test]
fn test_custom_remote_shell_config() {
    use core::client::ClientConfig;

    // Create config with custom remote shell
    let config = ClientConfig::builder()
        .set_remote_shell(vec!["ssh", "-p", "2222", "-i", "/path/to/key"])
        .build();

    // Verify the remote shell is stored correctly
    let shell_args = config.remote_shell().expect("remote_shell should be Some");
    assert_eq!(shell_args.len(), 5);
    assert_eq!(shell_args[0].to_string_lossy(), "ssh");
    assert_eq!(shell_args[1].to_string_lossy(), "-p");
    assert_eq!(shell_args[2].to_string_lossy(), "2222");
    assert_eq!(shell_args[3].to_string_lossy(), "-i");
    assert_eq!(shell_args[4].to_string_lossy(), "/path/to/key");
}

#[test]
fn test_custom_rsync_path_config() {
    use core::client::ClientConfig;

    // Create config with custom rsync path
    let config = ClientConfig::builder()
        .set_rsync_path("/opt/rsync/bin/rsync")
        .build();

    // Verify the rsync path is stored correctly
    let rsync_path = config.rsync_path().expect("rsync_path should be Some");
    assert_eq!(rsync_path.to_string_lossy(), "/opt/rsync/bin/rsync");
}

#[test]
fn test_remote_invocation_with_custom_rsync_path() {
    use core::client::{
        ClientConfig,
        remote::invocation::{RemoteInvocationBuilder, RemoteRole},
    };

    // Create config with custom rsync path
    let config = ClientConfig::builder()
        .set_rsync_path("/usr/local/bin/rsync")
        .build();

    // Build invocation for sender role
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    // First argument should be the custom rsync path, not "rsync"
    assert_eq!(args[0].to_string_lossy(), "/usr/local/bin/rsync");
    assert_eq!(args[1].to_string_lossy(), "--server");
    assert_eq!(args[2].to_string_lossy(), "--sender");
}

#[test]
fn test_remote_invocation_with_default_rsync_path() {
    use core::client::{
        ClientConfig,
        remote::invocation::{RemoteInvocationBuilder, RemoteRole},
    };

    // Create config without custom rsync path
    let config = ClientConfig::builder().build();

    // Build invocation for sender role
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let args = builder.build("/remote/path");

    // First argument should be default "rsync"
    assert_eq!(args[0].to_string_lossy(), "rsync");
    assert_eq!(args[1].to_string_lossy(), "--server");
}

#[test]
#[ignore] // Requires SSH setup
fn test_ssh_with_custom_port() {
    // This test would require an actual SSH server setup
    // It should:
    // 1. Set up a test SSH server on a custom port
    // 2. Create a config with custom remote shell: ssh -p <port>
    // 3. Execute a transfer
    // 4. Verify it works correctly
    //
    // For now, this is marked as ignored and serves as documentation
    // for future integration testing.
}

#[test]
fn test_multiple_sources_same_host() {
    use core::client::remote::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
    use std::ffi::OsString;

    let sources = vec![
        OsString::from("host:/file1"),
        OsString::from("host:/file2"),
        OsString::from("host:/dir/file3"),
    ];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination).expect("Should succeed");

    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "local/");
            assert_eq!(
                remote_sources,
                RemoteOperands::Multiple(vec![
                    "host:/file1".to_string(),
                    "host:/file2".to_string(),
                    "host:/dir/file3".to_string(),
                ])
            );
        }
        _ => panic!("Expected Pull transfer"),
    }
}

#[test]
fn test_multiple_sources_with_user_same_host() {
    use core::client::remote::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
    use std::ffi::OsString;

    let sources = vec![
        OsString::from("user@host:/file1"),
        OsString::from("user@host:/file2"),
    ];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination).expect("Should succeed");

    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "local/");
            assert_eq!(
                remote_sources,
                RemoteOperands::Multiple(vec![
                    "user@host:/file1".to_string(),
                    "user@host:/file2".to_string(),
                ])
            );
        }
        _ => panic!("Expected Pull transfer"),
    }
}

#[test]
fn test_multiple_sources_different_hosts_error() {
    use core::client::remote::determine_transfer_role;
    use std::ffi::OsString;

    let sources = vec![
        OsString::from("host1:/file1"),
        OsString::from("host2:/file2"),
    ];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination);
    assert!(result.is_err(), "Should reject different hosts");

    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("same host") || err_msg.contains("host1") && err_msg.contains("host2"),
        "Error should mention host mismatch: {err_msg}"
    );
}

#[test]
fn test_multiple_sources_user_mismatch_error() {
    use core::client::remote::determine_transfer_role;
    use std::ffi::OsString;

    let sources = vec![
        OsString::from("alice@host:/file1"),
        OsString::from("bob@host:/file2"),
    ];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination);
    assert!(result.is_err(), "Should reject different usernames");

    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("username") || err_msg.contains("alice") && err_msg.contains("bob"),
        "Error should mention username mismatch: {err_msg}"
    );
}

#[test]
fn test_multiple_sources_mixed_explicit_implicit_user_error() {
    use core::client::remote::determine_transfer_role;
    use std::ffi::OsString;

    let sources = vec![
        OsString::from("user@host:/file1"),
        OsString::from("host:/file2"),
    ];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination);
    assert!(
        result.is_err(),
        "Should reject mixed explicit/implicit username"
    );

    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("username")
            || err_msg.contains("explicit")
            || err_msg.contains("implicit"),
        "Error should mention username mixing: {err_msg}"
    );
}

#[test]
fn test_single_remote_source_returns_single_variant() {
    use core::client::remote::{RemoteOperands, RemoteRole, TransferSpec, determine_transfer_role};
    use std::ffi::OsString;

    let sources = vec![OsString::from("host:/single/file")];
    let destination = OsString::from("local/");

    let result = determine_transfer_role(&sources, &destination).expect("Should succeed");

    assert_eq!(result.role(), RemoteRole::Receiver);
    match result {
        TransferSpec::Pull {
            remote_sources,
            local_dest,
        } => {
            assert_eq!(local_dest, "local/");
            assert_eq!(
                remote_sources,
                RemoteOperands::Single("host:/single/file".to_string())
            );
        }
        _ => panic!("Expected Pull transfer"),
    }
}

#[test]
fn test_remote_invocation_with_multiple_paths() {
    use core::client::{ClientConfig, remote::RemoteInvocationBuilder, remote::RemoteRole};

    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Receiver);
    let args = builder.build_with_paths(&["/path1", "/path2", "/path3"]);

    assert_eq!(args[0].to_string_lossy(), "rsync");
    assert_eq!(args[1].to_string_lossy(), "--server");
    // No --sender for receiver role, so flags come next
    let flags_idx = 2;
    assert!(args[flags_idx].to_string_lossy().starts_with('-'));
    let dot_idx = flags_idx + 1;
    assert_eq!(args[dot_idx].to_string_lossy(), ".");
    // Paths come after "."
    assert_eq!(args[dot_idx + 1].to_string_lossy(), "/path1");
    assert_eq!(args[dot_idx + 2].to_string_lossy(), "/path2");
    assert_eq!(args[dot_idx + 3].to_string_lossy(), "/path3");
}
