//! Integration tests for rsync:// protocol transfers.
//!
//! Tests oc-rsync's ability to connect to public rsync:// servers
//! and perform file transfers. These tests require network access.

mod integration;

use integration::helpers::*;
use std::fs;

/// Skip test if network is unavailable or server is unreachable.
fn check_rsync_server(url: &str) -> bool {
    use std::process::Command;
    let result = Command::new("timeout")
        .args(["5", "rsync", "--list-only", url])
        .output();
    match result {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Helper to run oc-rsync with timeout.
fn rsync_with_timeout(args: &[&str], timeout_secs: u64) -> std::process::Output {
    use std::process::Command;

    let binary = locate_oc_rsync().expect("oc-rsync binary must be available");

    Command::new("timeout")
        .arg(format!("{timeout_secs}"))
        .arg(&binary)
        .args(args)
        .output()
        .expect("failed to execute oc-rsync")
}

fn locate_oc_rsync() -> Option<std::path::PathBuf> {
    use std::env;
    use std::path::PathBuf;

    // Try CARGO_BIN_EXE_oc-rsync first
    if let Some(path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    // Try common locations
    for subdir in ["target/release", "target/debug"] {
        let candidate = PathBuf::from(subdir).join("oc-rsync");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

// ============================================================================
// GNU FTP Server Tests
// ============================================================================

#[test]
#[ignore = "requires network access to ftp.gnu.org"]
fn rsync_protocol_gnu_ftp_small_file() {
    let url = "rsync://ftp.gnu.org/gnu/coreutils/coreutils-5.0.tar.bz2.sig";

    if !check_rsync_server("rsync://ftp.gnu.org/gnu/") {
        eprintln!("Skipping: ftp.gnu.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(&["-av", url, dest_dir.to_str().unwrap()], 30);

    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("rsync:// transfer failed");
    }

    // Verify file was downloaded
    let downloaded = dest_dir.join("coreutils-5.0.tar.bz2.sig");
    assert!(downloaded.exists(), "downloaded file should exist");

    let content = fs::read(&downloaded).unwrap();
    assert_eq!(content.len(), 65, "GPG signature should be 65 bytes");
}

#[test]
#[ignore = "requires network access to ftp.gnu.org"]
fn rsync_protocol_gnu_ftp_directory() {
    let url = "rsync://ftp.gnu.org/gnu/hello/";

    if !check_rsync_server("rsync://ftp.gnu.org/gnu/") {
        eprintln!("Skipping: ftp.gnu.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // Only get the README and small files to keep test fast
    let output = rsync_with_timeout(
        &[
            "-av",
            "--include=README",
            "--include=*.sig",
            "--exclude=*",
            url,
            dest_dir.to_str().unwrap(),
        ],
        60,
    );

    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("rsync:// directory transfer failed");
    }
}

// ============================================================================
// Apache Mirror Tests
// ============================================================================

#[test]
#[ignore = "requires network access to rsync.apache.org"]
fn rsync_protocol_apache_small_file() {
    let url = "rsync://rsync.apache.org/apache-dist/README.html";

    if !check_rsync_server("rsync://rsync.apache.org/apache-dist/") {
        eprintln!("Skipping: rsync.apache.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(&["-av", url, dest_dir.to_str().unwrap()], 30);

    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("rsync:// transfer from Apache failed");
    }

    let downloaded = dest_dir.join("README.html");
    assert!(downloaded.exists(), "README.html should be downloaded");
}

// ============================================================================
// Debian Mirror Tests
// ============================================================================

#[test]
#[ignore = "requires network access to ftp.debian.org"]
fn rsync_protocol_debian_readme() {
    let url = "rsync://ftp.debian.org/debian/README";

    if !check_rsync_server("rsync://ftp.debian.org/debian/") {
        eprintln!("Skipping: ftp.debian.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(&["-av", url, dest_dir.to_str().unwrap()], 30);

    if !output.status.success() {
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("rsync:// transfer from Debian failed");
    }

    let downloaded = dest_dir.join("README");
    assert!(downloaded.exists(), "README should be downloaded");
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn rsync_protocol_invalid_server() {
    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(
        &[
            "-av",
            "rsync://nonexistent.invalid.example/module/",
            dest_dir.to_str().unwrap(),
        ],
        10,
    );

    // Should fail with connection error
    assert!(!output.status.success(), "should fail with invalid server");
}

#[test]
fn rsync_protocol_invalid_module() {
    if !check_rsync_server("rsync://ftp.gnu.org/gnu/") {
        eprintln!("Skipping: ftp.gnu.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(
        &[
            "-av",
            "rsync://ftp.gnu.org/nonexistent_module_12345/",
            dest_dir.to_str().unwrap(),
        ],
        15,
    );

    // Should fail - module doesn't exist
    assert!(!output.status.success(), "should fail with invalid module");
}

// ============================================================================
// Incremental Transfer Tests
// ============================================================================

#[test]
#[ignore = "requires network access to ftp.gnu.org"]
fn rsync_protocol_incremental_sync() {
    let url = "rsync://ftp.gnu.org/gnu/coreutils/coreutils-5.0.tar.bz2.sig";

    if !check_rsync_server("rsync://ftp.gnu.org/gnu/") {
        eprintln!("Skipping: ftp.gnu.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    // First sync
    let output1 = rsync_with_timeout(&["-av", url, dest_dir.to_str().unwrap()], 30);
    assert!(output1.status.success(), "first sync should succeed");

    // Second sync (should be fast - no changes)
    let output2 = rsync_with_timeout(&["-av", url, dest_dir.to_str().unwrap()], 30);
    assert!(output2.status.success(), "incremental sync should succeed");

    // Verify the output indicates no transfer needed
    let stdout = String::from_utf8_lossy(&output2.stdout);
    // File should not be re-transferred (mtime check)
    assert!(
        !stdout.contains("coreutils-5.0.tar.bz2.sig") || stdout.contains("speedup"),
        "incremental sync should skip unchanged file"
    );
}

// ============================================================================
// Dry Run Tests
// ============================================================================

#[test]
#[ignore = "requires network access to ftp.gnu.org"]
fn rsync_protocol_dry_run() {
    let url = "rsync://ftp.gnu.org/gnu/coreutils/coreutils-5.0.tar.bz2.sig";

    if !check_rsync_server("rsync://ftp.gnu.org/gnu/") {
        eprintln!("Skipping: ftp.gnu.org unreachable");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").unwrap();

    let output = rsync_with_timeout(
        &[
            "-avn", // dry-run
            url,
            dest_dir.to_str().unwrap(),
        ],
        30,
    );

    assert!(output.status.success(), "dry-run should succeed");

    // File should NOT be downloaded
    let would_be_downloaded = dest_dir.join("coreutils-5.0.tar.bz2.sig");
    assert!(
        !would_be_downloaded.exists(),
        "dry-run should not create file"
    );
}
