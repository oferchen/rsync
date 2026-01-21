//! Upstream rsync client to oc-rsync daemon interoperability tests.
//!
//! These tests verify that upstream rsync clients can successfully connect to
//! and transfer files with the oc-rsync daemon across different protocol versions.
//!
//! Test strategy:
//! 1. Start oc-rsync daemon on a test port with test configuration
//! 2. Use upstream rsync client binaries to connect and transfer files
//! 3. Verify protocol negotiation, file transfers, and metadata preservation
//!
//! Upstream reference:
//! - `target/interop/upstream-src/rsync-3.4.1/clientserver.c` - client protocol
//! - `target/interop/upstream-src/rsync-3.4.1/main.c:1267-1384` - client_run()
//!
//! oc-rsync reference:
//! - `crates/daemon/src/daemon/` - daemon implementation
//! - `crates/core/src/server/` - server-side protocol handling
//! - `crates/protocol/src/negotiation/` - handshake and version negotiation

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use tempfile::{TempDir, tempdir};

// Upstream rsync client binary paths
const UPSTREAM_3_4_1: &str = "/home/ofer/rsync/target/interop/upstream-install/3.4.1/bin/rsync";
const UPSTREAM_3_1_3: &str = "/home/ofer/rsync/target/interop/upstream-install/3.1.3/bin/rsync";
const UPSTREAM_3_0_9: &str = "/home/ofer/rsync/target/interop/upstream-install/3.0.9/bin/rsync";

// oc-rsync daemon binary path
const OC_RSYNC_BINARY: &str = "/home/ofer/rsync/target/release/oc-rsync";
const OC_RSYNC_DEBUG_BINARY: &str = "/home/ofer/rsync/target/debug/oc-rsync";

/// Get the path to oc-rsync binary, preferring release over debug.
fn oc_rsync_binary() -> &'static str {
    if Path::new(OC_RSYNC_BINARY).exists() {
        OC_RSYNC_BINARY
    } else if Path::new(OC_RSYNC_DEBUG_BINARY).exists() {
        OC_RSYNC_DEBUG_BINARY
    } else {
        OC_RSYNC_BINARY // Return release path and let caller fail with clear error
    }
}

/// Test infrastructure for managing oc-rsync daemon instances.
struct OcDaemon {
    _workdir: TempDir,
    #[allow(dead_code)]
    config_path: PathBuf,
    log_path: PathBuf,
    #[allow(dead_code)]
    pid_path: PathBuf,
    module_path: PathBuf,
    port: u16,
    process: Option<Child>,
}

impl OcDaemon {
    /// Start an oc-rsync daemon for testing.
    ///
    /// Creates a temporary directory structure with:
    /// - Configuration file (oc-rsyncd.conf)
    /// - Log file
    /// - PID file
    /// - Module directory for file storage
    fn start(port: u16) -> std::io::Result<Self> {
        let workdir = tempdir()?;
        let config_path = workdir.path().join("oc-rsyncd.conf");
        let log_path = workdir.path().join("daemon.log");
        let pid_path = workdir.path().join("daemon.pid");
        let module_path = workdir.path().join("testmodule");

        fs::create_dir_all(&module_path)?;

        // Write oc-rsyncd.conf matching daemon configuration format
        // Uses oc-rsync branding conventions per CLAUDE.md
        // Note: port and log-file are passed via CLI, not config
        // use chroot and numeric ids go in module section
        let config_content = format!(
            "\
# Test daemon configuration for interop tests
pid file = {}

[testmodule]
    path = {}
    comment = Test module for upstream client interop
    read only = false
    list = yes
    use chroot = false
    numeric ids = yes
",
            pid_path.display(),
            module_path.display()
        );
        fs::write(&config_path, config_content)?;

        let binary = oc_rsync_binary();
        if !Path::new(binary).exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("oc-rsync binary not found at: {binary}"),
            ));
        }

        // Start daemon with --no-detach so we can manage the process lifecycle
        // Mirrors upstream rsync daemon invocation from daemon_client_interop.rs
        // Pass port and log-file via CLI since they're not config directives
        let mut child = Command::new(binary)
            .arg("--daemon")
            .arg("--config")
            .arg(&config_path)
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--log-file")
            .arg(&log_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Give daemon time to initialize and bind to port
        // Matches startup delay from daemon_client_interop.rs
        thread::sleep(Duration::from_millis(500));

        // Verify daemon is still running (didn't crash on startup)
        if let Some(status) = child.try_wait()? {
            let stderr = child.stderr.take();
            let mut error_msg = format!("oc-rsync daemon exited immediately with status: {status}");
            if let Some(mut stderr) = stderr {
                let mut buf = String::new();
                if stderr.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                    error_msg.push_str(&format!("\nStderr: {buf}"));
                }
            }
            return Err(std::io::Error::other(error_msg));
        } else {
            // Still running - daemon started successfully
        }

        Ok(Self {
            _workdir: workdir,
            config_path,
            log_path,
            pid_path,
            module_path,
            port,
            process: Some(child),
        })
    }

    /// Get the rsync:// URL for connecting to this daemon.
    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/testmodule", self.port)
    }

    /// Get the module root directory path for file operations.
    fn module_path(&self) -> &Path {
        &self.module_path
    }

    /// Get the daemon log contents for debugging test failures.
    #[allow(dead_code)]
    fn log_contents(&self) -> std::io::Result<String> {
        fs::read_to_string(&self.log_path)
    }

    /// Wait for daemon to be ready by attempting TCP connection to the port.
    ///
    /// This ensures the daemon is fully initialized before clients connect.
    fn wait_ready(&self, timeout: Duration) -> std::io::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                // Capture log contents for debugging timeout failures
                let log = self
                    .log_contents()
                    .unwrap_or_else(|_| String::from("(log unavailable)"));
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("daemon did not become ready within {timeout:?}\nLog: {log}"),
                ));
            }

            match TcpStream::connect(format!("127.0.0.1:{}", self.port)) {
                Ok(_) => return Ok(()),
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

impl Drop for OcDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Helper to create test files with specific content.
///
/// Creates parent directories as needed.
fn create_test_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write test file");
}

// ============================================================================
// Daemon Startup and Basic Connectivity Tests
// ============================================================================

/// Test that oc-rsync daemon starts successfully and accepts connections.
///
/// This is a smoke test to verify basic daemon functionality before
/// running protocol-specific tests.
#[test]
#[ignore = "requires oc-rsync binary"]
fn test_oc_daemon_starts_and_accepts_connections() {
    let daemon = OcDaemon::start(18873).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon should become ready");

    // Verify we can connect to the daemon port
    let _stream = TcpStream::connect(format!("127.0.0.1:{}", daemon.port))
        .expect("should connect to daemon port");
}

/// Test that oc-rsync daemon responds with proper greeting.
///
/// Verifies the daemon implements the @RSYNCD: protocol greeting correctly.
/// Upstream reference: clientserver.c:125-144 (daemon greeting format)
#[test]
#[ignore = "requires oc-rsync binary"]
fn test_oc_daemon_sends_protocol_greeting() {
    let daemon = OcDaemon::start(18874).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let mut reader = BufReader::new(stream);
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");

    assert!(
        greeting.starts_with("@RSYNCD: "),
        "daemon should send @RSYNCD: greeting, got: {greeting}"
    );

    // Parse protocol version from greeting
    let version_part = greeting
        .trim()
        .strip_prefix("@RSYNCD: ")
        .expect("parse version");

    // oc-rsync should advertise protocol 31 (matches rsync 3.4.x)
    // See: crates/protocol/src/version.rs
    let protocol_version: f64 = version_part
        .split_whitespace()
        .next()
        .expect("get version number")
        .parse()
        .expect("parse protocol version");

    assert!(
        (30.0..=32.0).contains(&protocol_version),
        "oc-rsync should advertise protocol 30-32, got: {protocol_version}"
    );
}

/// Test daemon shutdown and cleanup.
///
/// Verifies that daemon process terminates cleanly when dropped.
#[test]
#[ignore = "requires oc-rsync binary"]
fn test_oc_daemon_shutdown_cleanup() {
    let port = 18875;

    {
        let daemon = OcDaemon::start(port).expect("start daemon");
        daemon
            .wait_ready(Duration::from_secs(5))
            .expect("daemon ready");
        // Daemon dropped here
    }

    // Give OS time to release port
    thread::sleep(Duration::from_millis(100));

    // Port should be available again after daemon cleanup
    let result = TcpStream::connect(format!("127.0.0.1:{port}"));
    assert!(
        result.is_err(),
        "port should be unbound after daemon shutdown"
    );
}

// ============================================================================
// Upstream Client Compatibility Tests (Protocol Negotiation)
// ============================================================================

/// Test upstream rsync 3.4.1 client connecting to oc-rsync daemon.
///
/// Verifies protocol negotiation and basic handshake with newest upstream client.
/// Protocol 31 is the target for interop.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_upstream_3_4_1_client_handshake() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found at {UPSTREAM_3_4_1}");
        return;
    }

    let daemon = OcDaemon::start(18876).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create test file for pull operation
    create_test_file(
        &daemon.module_path().join("test.txt"),
        b"test content from oc-rsync daemon",
    );

    let dest_dir = tempdir().expect("create dest dir");

    // Use upstream 3.4.1 client to connect to oc-rsync daemon
    let output = Command::new(UPSTREAM_3_4_1)
        .arg("-v") // Verbose to see protocol version
        .arg("--timeout=10")
        .arg(format!("{}/test.txt", daemon.url()))
        .arg(dest_dir.path())
        .output()
        .expect("run upstream 3.4.1 client");

    if !output.status.success() {
        eprintln!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("upstream 3.4.1 → oc-rsync daemon should succeed");
    }

    // Verify file was transferred
    let transferred = fs::read(dest_dir.path().join("test.txt")).expect("read transferred file");
    assert_eq!(transferred, b"test content from oc-rsync daemon");
}

/// Test upstream rsync 3.1.3 client connecting to oc-rsync daemon.
///
/// Verifies backward compatibility with older protocol (30/31).
/// This is a common production version still in wide use.
#[test]
#[ignore = "requires upstream rsync 3.1.3 and oc-rsync binary"]
fn test_upstream_3_1_3_client_handshake() {
    if !Path::new(UPSTREAM_3_1_3).exists() {
        eprintln!("Skipping: upstream rsync 3.1.3 not found at {UPSTREAM_3_1_3}");
        return;
    }

    let daemon = OcDaemon::start(18877).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("legacy.txt"),
        b"content for 3.1.3 client test",
    );

    let dest_dir = tempdir().expect("create dest dir");

    let output = Command::new(UPSTREAM_3_1_3)
        .arg("-v")
        .arg("--timeout=10")
        .arg(format!("{}/legacy.txt", daemon.url()))
        .arg(dest_dir.path())
        .output()
        .expect("run upstream 3.1.3 client");

    if !output.status.success() {
        eprintln!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("upstream 3.1.3 → oc-rsync daemon should succeed");
    }

    let transferred = fs::read(dest_dir.path().join("legacy.txt")).expect("read transferred file");
    assert_eq!(transferred, b"content for 3.1.3 client test");
}

/// Test upstream rsync 3.0.9 client connecting to oc-rsync daemon.
///
/// Verifies backward compatibility with much older protocol (30).
/// This is the oldest version commonly tested for interop.
#[test]
#[ignore = "requires upstream rsync 3.0.9 and oc-rsync binary"]
fn test_upstream_3_0_9_client_handshake() {
    if !Path::new(UPSTREAM_3_0_9).exists() {
        eprintln!("Skipping: upstream rsync 3.0.9 not found at {UPSTREAM_3_0_9}");
        return;
    }

    let daemon = OcDaemon::start(18878).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("old.txt"),
        b"content for 3.0.9 ancient client",
    );

    let dest_dir = tempdir().expect("create dest dir");

    let output = Command::new(UPSTREAM_3_0_9)
        .arg("-v")
        .arg("--timeout=10")
        .arg(format!("{}/old.txt", daemon.url()))
        .arg(dest_dir.path())
        .output()
        .expect("run upstream 3.0.9 client");

    if !output.status.success() {
        eprintln!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("upstream 3.0.9 → oc-rsync daemon should succeed");
    }

    let transferred = fs::read(dest_dir.path().join("old.txt")).expect("read transferred file");
    assert_eq!(transferred, b"content for 3.0.9 ancient client");
}

// ============================================================================
// File Transfer Tests (Pull from oc-rsync daemon)
// ============================================================================

/// Test upstream client pulling single file from oc-rsync daemon.
///
/// Baseline test for file transfer functionality.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_single_file_from_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18879).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("single.txt"),
        b"single file content",
    );

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/single.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "transfer should succeed");

    assert_eq!(
        fs::read(dest_dir.path().join("single.txt")).expect("read file"),
        b"single file content"
    );
}

/// Test upstream client pulling directory tree from oc-rsync daemon.
///
/// Verifies recursive transfer and directory structure preservation.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_directory_tree_from_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18880).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create nested directory structure
    create_test_file(&daemon.module_path().join("root.txt"), b"root file");
    create_test_file(
        &daemon.module_path().join("dir1/file1.txt"),
        b"file in dir1",
    );
    create_test_file(
        &daemon.module_path().join("dir1/file2.txt"),
        b"another in dir1",
    );
    create_test_file(
        &daemon.module_path().join("dir1/subdir/deep.txt"),
        b"deeply nested",
    );
    create_test_file(
        &daemon.module_path().join("dir2/file3.txt"),
        b"file in dir2",
    );

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "recursive transfer should succeed");

    // Verify all files were transferred
    assert_eq!(
        fs::read(dest_dir.path().join("root.txt")).expect("read root.txt"),
        b"root file"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("dir1/file1.txt")).expect("read file1"),
        b"file in dir1"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("dir1/file2.txt")).expect("read file2"),
        b"another in dir1"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("dir1/subdir/deep.txt")).expect("read deep"),
        b"deeply nested"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("dir2/file3.txt")).expect("read file3"),
        b"file in dir2"
    );
}

/// Test upstream client pulling large file from oc-rsync daemon.
///
/// Verifies delta transfer and block-based transfer for larger files.
/// Uses 1MB+ file to ensure multi-block transfer.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_large_file_from_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18881).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create 1MB file with predictable pattern
    let large_content: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
    create_test_file(&daemon.module_path().join("large.bin"), &large_content);

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=30") // Longer timeout for large transfer
        .arg(format!("{}/large.bin", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "large file transfer should succeed");

    let transferred = fs::read(dest_dir.path().join("large.bin")).expect("read large file");
    assert_eq!(transferred.len(), large_content.len(), "size should match");
    assert_eq!(transferred, large_content, "content should match");
}

/// Test upstream client pulling files with various special characters.
///
/// Verifies path encoding and special character handling.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_files_with_special_chars_from_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18882).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create files with spaces and other allowed special characters
    create_test_file(
        &daemon.module_path().join("file with spaces.txt"),
        b"spaces",
    );
    create_test_file(
        &daemon.module_path().join("file-with-dashes.txt"),
        b"dashes",
    );
    create_test_file(
        &daemon.module_path().join("file_with_underscores.txt"),
        b"underscores",
    );

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(
        status.success(),
        "transfer with special chars should succeed"
    );

    assert_eq!(
        fs::read(dest_dir.path().join("file with spaces.txt")).expect("read spaces"),
        b"spaces"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("file-with-dashes.txt")).expect("read dashes"),
        b"dashes"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("file_with_underscores.txt")).expect("read underscores"),
        b"underscores"
    );
}

// ============================================================================
// File Transfer Tests (Push to oc-rsync daemon)
// ============================================================================

/// Test upstream client pushing single file to oc-rsync daemon.
///
/// Verifies reverse transfer direction (client → daemon).
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_push_single_file_to_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18883).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let source_dir = tempdir().expect("create source dir");
    create_test_file(&source_dir.path().join("upload.txt"), b"uploaded content");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(source_dir.path().join("upload.txt"))
        .arg(format!("{}/", daemon.url()))
        .status()
        .expect("run upstream client");

    assert!(status.success(), "push should succeed");

    // Verify file appeared in daemon module
    assert_eq!(
        fs::read(daemon.module_path().join("upload.txt")).expect("read uploaded"),
        b"uploaded content"
    );
}

/// Test upstream client pushing directory tree to oc-rsync daemon.
///
/// Verifies recursive push and directory creation on daemon side.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_push_directory_tree_to_oc_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18884).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let source_dir = tempdir().expect("create source dir");
    create_test_file(&source_dir.path().join("file1.txt"), b"first");
    create_test_file(&source_dir.path().join("subdir/file2.txt"), b"second");
    create_test_file(&source_dir.path().join("subdir/deep/file3.txt"), b"third");

    let mut source_path = source_dir.path().as_os_str().to_os_string();
    source_path.push("/");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(&source_path)
        .arg(format!("{}/", daemon.url()))
        .status()
        .expect("run upstream client");

    assert!(status.success(), "push directory should succeed");

    // Verify all files appeared in daemon module
    assert_eq!(
        fs::read(daemon.module_path().join("file1.txt")).expect("read file1"),
        b"first"
    );
    assert_eq!(
        fs::read(daemon.module_path().join("subdir/file2.txt")).expect("read file2"),
        b"second"
    );
    assert_eq!(
        fs::read(daemon.module_path().join("subdir/deep/file3.txt")).expect("read file3"),
        b"third"
    );
}

// ============================================================================
// Metadata Preservation Tests
// ============================================================================

/// Test that file permissions are preserved during transfer.
///
/// Verifies -a/--archive mode preserves Unix permissions.
#[test]
#[cfg(unix)]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18885).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let test_file = daemon.module_path().join("perms.txt");
    create_test_file(&test_file, b"permission test");

    // Set specific permissions (rwxr-xr--)
    let perms = fs::Permissions::from_mode(0o754);
    fs::set_permissions(&test_file, perms).expect("set permissions");

    let original_mode = fs::metadata(&test_file)
        .expect("read metadata")
        .permissions()
        .mode();

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av") // Archive mode preserves permissions
        .arg("--timeout=10")
        .arg(format!("{}/perms.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "transfer should succeed");

    let transferred_mode = fs::metadata(dest_dir.path().join("perms.txt"))
        .expect("read transferred metadata")
        .permissions()
        .mode();

    assert_eq!(
        transferred_mode & 0o777,
        original_mode & 0o777,
        "permissions should be preserved"
    );
}

/// Test that modification times are preserved during transfer.
///
/// Verifies -a/--archive mode preserves mtime.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_preserves_mtime() {
    use std::time::SystemTime;

    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18886).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let test_file = daemon.module_path().join("mtime.txt");
    create_test_file(&test_file, b"mtime test");

    // Set file to a specific time in the past (1 hour ago)
    let past_time = SystemTime::now() - Duration::from_secs(3600);
    let filetime = filetime::FileTime::from_system_time(past_time);
    filetime::set_file_mtime(&test_file, filetime).expect("set mtime");

    let original_mtime = fs::metadata(&test_file)
        .expect("read metadata")
        .modified()
        .expect("get mtime");

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av") // Archive mode preserves times
        .arg("--timeout=10")
        .arg(format!("{}/mtime.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "transfer should succeed");

    let transferred_mtime = fs::metadata(dest_dir.path().join("mtime.txt"))
        .expect("read transferred metadata")
        .modified()
        .expect("get transferred mtime");

    // Allow 2 second tolerance for filesystem granularity
    let diff = if transferred_mtime > original_mtime {
        transferred_mtime.duration_since(original_mtime).unwrap()
    } else {
        original_mtime.duration_since(transferred_mtime).unwrap()
    };

    assert!(
        diff < Duration::from_secs(2),
        "mtime should be preserved (diff: {diff:?})"
    );
}

// ============================================================================
// Protocol-Level Tests
// ============================================================================

/// Test module listing (#list request) from upstream client.
///
/// Verifies oc-rsync daemon correctly responds to module list requests.
/// Upstream reference: clientserver.c:210-245 (module listing)
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_module_listing_from_upstream_client() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18887).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Request module list (no module path specified)
    let output = Command::new(UPSTREAM_3_4_1)
        .arg("--timeout=10")
        .arg(format!("rsync://127.0.0.1:{}/", daemon.port))
        .output()
        .expect("run upstream client");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should list testmodule
    assert!(
        stdout.contains("testmodule"),
        "module list should contain testmodule, got: {stdout}"
    );
}

/// Test manual protocol handshake with oc-rsync daemon.
///
/// Verifies low-level protocol implementation directly.
/// This is similar to test_oc_daemon_sends_protocol_greeting but goes further
/// into the handshake sequence.
#[test]
#[ignore = "requires oc-rsync binary"]
fn test_manual_protocol_handshake_with_oc_daemon() {
    let daemon = OcDaemon::start(18888).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Step 1: Receive daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(
        greeting.starts_with("@RSYNCD: "),
        "expected greeting, got: {greeting}"
    );

    // Step 2: Send client version with auth digests (protocol 30+ format)
    writer_stream
        .write_all(b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send client version");
    writer_stream.flush().expect("flush");

    // Step 3: Request module
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module request");
    writer_stream.flush().expect("flush");

    // Step 4: Expect @RSYNCD: OK (may have MOTD lines first)
    let mut got_ok = false;
    for _ in 0..10 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response");
        let trimmed = line.trim();

        if trimmed == "@RSYNCD: OK" {
            got_ok = true;
            break;
        }

        // Fail fast on errors
        assert!(
            !trimmed.starts_with("@ERROR"),
            "unexpected error: {trimmed}"
        );
    }

    assert!(got_ok, "should receive @RSYNCD: OK");
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test error when upstream client requests non-existent module.
///
/// Verifies oc-rsync daemon sends proper @ERROR response.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_error_nonexistent_module_from_upstream_client() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18889).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let output = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("rsync://127.0.0.1:{}/nonexistent/", daemon.port))
        .arg("/tmp/")
        .output()
        .expect("run upstream client");

    // Should fail
    assert!(
        !output.status.success(),
        "should fail for nonexistent module"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown module") || stderr.contains("module") || stderr.contains("error"),
        "error should mention module issue, got: {stderr}"
    );
}

/// Test error when upstream client tries to write to read-only module.
///
/// Verifies oc-rsync daemon enforces read-only restrictions.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary - needs read-only module config"]
fn test_error_write_to_readonly_module() {
    // This test would require a read-only module configuration.
    // Current test daemon has read only = false for testmodule.
    //
    // To implement:
    // 1. Create daemon with additional read-only module
    // 2. Try to push file to that module
    // 3. Verify @ERROR response or transfer rejection
}

/// Test connection timeout when daemon is not responding.
#[test]
fn test_error_connection_refused() {
    // Try to connect to port that's not listening
    let result = TcpStream::connect("127.0.0.1:1");
    assert!(result.is_err(), "connection to closed port should fail");
}

// ============================================================================
// Compression and Checksum Algorithm Tests
// ============================================================================

/// Test transfer with compression enabled.
///
/// Verifies oc-rsync daemon negotiates and handles compressed transfers.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_with_compression() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18890).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create compressible file (repetitive content)
    let compressible: Vec<u8> = b"AAAA".iter().cycle().take(10000).copied().collect();
    create_test_file(&daemon.module_path().join("compress.txt"), &compressible);

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-avz") // Enable compression
        .arg("--timeout=10")
        .arg(format!("{}/compress.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client with compression");

    if !status.success() {
        eprintln!("=== Daemon log ===");
        if let Ok(log) = daemon.log_contents() {
            eprintln!("{log}");
        }
        eprintln!("=== End daemon log ===");
        panic!("compressed transfer should succeed");
    }

    let transferred = fs::read(dest_dir.path().join("compress.txt")).expect("read file");
    assert_eq!(
        transferred, compressible,
        "content should match after compression"
    );
}

/// Test transfer with specific checksum algorithm.
///
/// Verifies oc-rsync daemon supports checksum negotiation (protocol 30+).
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_with_checksum_algorithm() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18891).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("checksum.txt"),
        b"content for checksum test",
    );

    let dest_dir = tempdir().expect("create dest dir");

    // Request specific checksum algorithm (md5)
    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--checksum-choice=md5")
        .arg("--timeout=10")
        .arg(format!("{}/checksum.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client with md5");

    assert!(
        status.success(),
        "transfer with md5 checksum should succeed"
    );

    assert_eq!(
        fs::read(dest_dir.path().join("checksum.txt")).expect("read file"),
        b"content for checksum test"
    );
}

// ============================================================================
// Stress and Edge Case Tests
// ============================================================================

/// Test transfer of many small files.
///
/// Verifies oc-rsync daemon handles file list overhead efficiently.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_many_small_files() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18892).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create 100 small files
    for i in 0..100 {
        create_test_file(
            &daemon.module_path().join(format!("small_{i:03}.txt")),
            format!("content {i}").as_bytes(),
        );
    }

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=30")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "many small files transfer should succeed");

    // Verify a sampling of files
    assert_eq!(
        fs::read(dest_dir.path().join("small_000.txt")).expect("read small_000"),
        b"content 0"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("small_050.txt")).expect("read small_050"),
        b"content 50"
    );
    assert_eq!(
        fs::read(dest_dir.path().join("small_099.txt")).expect("read small_099"),
        b"content 99"
    );
}

/// Test transfer of empty file.
///
/// Verifies oc-rsync daemon handles zero-length files correctly.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_empty_file() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18893).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(&daemon.module_path().join("empty.txt"), b"");

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/empty.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "empty file transfer should succeed");

    let transferred = fs::read(dest_dir.path().join("empty.txt")).expect("read empty file");
    assert_eq!(transferred.len(), 0, "empty file should remain empty");
}

/// Test transfer of file with only whitespace.
///
/// Edge case for content handling.
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn test_pull_whitespace_only_file() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = OcDaemon::start(18894).expect("start oc-rsync daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let whitespace_content = b"   \n\t\t\n   \n";
    create_test_file(
        &daemon.module_path().join("whitespace.txt"),
        whitespace_content,
    );

    let dest_dir = tempdir().expect("create dest dir");

    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/whitespace.txt", daemon.url()))
        .arg(dest_dir.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "whitespace file transfer should succeed");

    assert_eq!(
        fs::read(dest_dir.path().join("whitespace.txt")).expect("read whitespace"),
        whitespace_content
    );
}
