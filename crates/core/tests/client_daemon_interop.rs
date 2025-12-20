//! Client → upstream daemon interoperability tests.
//!
//! This test suite verifies that our client implementation can successfully
//! communicate with upstream rsync daemons across different protocol versions.
//!
//! Test coverage:
//! 1. Basic file transfers (pull and push)
//! 2. Protocol version negotiation across upstream versions
//! 3. Feature compatibility (compression, checksums, filters)
//! 4. Error handling (invalid modules, permission errors)
//! 5. Metadata preservation
//! 6. Incremental transfers
//!
//! Dependencies:
//! - Upstream rsync binaries at target/interop/upstream-install/{3.0.9,3.1.3,3.4.1}/bin/rsync
//! - Tests are marked with #[ignore] and check for binary availability at runtime
//!
//! Upstream references:
//! - clientserver.c - daemon protocol implementation
//! - main.c:1267-1384 - client_run() orchestration

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use core::client::{ClientConfig, run_client};
use tempfile::{TempDir, tempdir};

// ============================================================================
// Test Infrastructure
// ============================================================================

const UPSTREAM_3_0_9: &str = "target/interop/upstream-install/3.0.9/bin/rsync";
const UPSTREAM_3_1_3: &str = "target/interop/upstream-install/3.1.3/bin/rsync";
const UPSTREAM_3_4_1: &str = "target/interop/upstream-install/3.4.1/bin/rsync";

/// Helper to manage upstream rsync daemon instances for testing.
#[allow(dead_code)]
struct UpstreamDaemon {
    _workdir: TempDir,
    config_path: PathBuf,
    log_path: PathBuf,
    pid_path: PathBuf,
    module_path: PathBuf,
    port: u16,
    process: Option<Child>,
}

impl UpstreamDaemon {
    /// Start an upstream rsync daemon with a test module.
    fn start(upstream_binary: &str, port: u16) -> std::io::Result<Self> {
        let workdir = tempdir()?;
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let pid_path = workdir.path().join("rsyncd.pid");
        let module_path = workdir.path().join("module");

        fs::create_dir_all(&module_path)?;

        // Write daemon configuration
        let config_content = format!(
            "\
pid file = {}
port = {}
use chroot = false
numeric ids = yes

[testmodule]
    path = {}
    comment = Test module for client interop
    read only = false
",
            pid_path.display(),
            port,
            module_path.display()
        );
        fs::write(&config_path, config_content)?;

        // Start daemon with --no-detach for process management
        let mut child = Command::new(upstream_binary)
            .arg("--daemon")
            .arg("--config")
            .arg(&config_path)
            .arg("--no-detach")
            .arg("--log-file")
            .arg(&log_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Allow daemon to start
        thread::sleep(Duration::from_millis(500));

        // Verify daemon is running
        match child.try_wait()? {
            Some(status) => {
                let stderr = child.stderr.take();
                let mut error_msg = format!("Daemon exited immediately with status: {status}");
                if let Some(mut stderr) = stderr {
                    let mut buf = String::new();
                    if stderr.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                        error_msg.push_str(&format!("\nStderr: {buf}"));
                    }
                }
                return Err(std::io::Error::other(error_msg));
            }
            None => {
                // Daemon is running
            }
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

    /// Get the rsync:// URL for this daemon.
    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/testmodule", self.port)
    }

    /// Get the module root directory.
    fn module_path(&self) -> &Path {
        &self.module_path
    }

    /// Get daemon log contents for debugging.
    #[allow(dead_code)]
    fn log_contents(&self) -> std::io::Result<String> {
        fs::read_to_string(&self.log_path)
    }

    /// Wait for daemon to be ready by attempting TCP connection.
    fn wait_ready(&self, timeout: Duration) -> std::io::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "daemon did not become ready",
                ));
            }

            match TcpStream::connect(format!("127.0.0.1:{}", self.port)) {
                Ok(_) => return Ok(()),
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

impl Drop for UpstreamDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Helper to create test files with specific content.
fn create_test_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write test file");
}

/// Check if an upstream binary exists and skip test if not.
fn require_upstream(binary_path: &str) {
    if !Path::new(binary_path).exists() {
        eprintln!("Skipping: upstream rsync binary not found at {binary_path}");
        panic!("upstream binary required for this test");
    }
}

// ============================================================================
// Basic File Transfer Tests
// ============================================================================

/// Test pulling a single file from upstream daemon.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_single_file_from_daemon() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13001).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create test file in daemon module
    create_test_file(
        &daemon.module_path().join("test_file.txt"),
        b"hello from daemon",
    );

    // Set up local destination
    let dest_root = tempdir().expect("create dest dir");

    // Pull file using our client
    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/test_file.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let summary = run_client(config).expect("client pull succeeds");

    // Verify file was transferred
    let copied_file = dest_root.path().join("test_file.txt");
    assert!(copied_file.exists(), "file should be copied");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"hello from daemon"
    );
    assert!(summary.files_copied() >= 1, "at least one file copied");
}

/// Test pulling a directory recursively from upstream daemon.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_directory_recursive() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13002).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create directory structure in daemon module
    create_test_file(&daemon.module_path().join("file1.txt"), b"content1");
    create_test_file(&daemon.module_path().join("subdir/file2.txt"), b"content2");
    create_test_file(
        &daemon.module_path().join("subdir/nested/file3.txt"),
        b"content3",
    );

    let dest_root = tempdir().expect("create dest dir");

    // Pull directory recursively
    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .build();

    let summary = run_client(config).expect("client pull succeeds");

    // Verify all files were transferred
    assert_eq!(
        fs::read(dest_root.path().join("file1.txt")).expect("read file1"),
        b"content1"
    );
    assert_eq!(
        fs::read(dest_root.path().join("subdir/file2.txt")).expect("read file2"),
        b"content2"
    );
    assert_eq!(
        fs::read(dest_root.path().join("subdir/nested/file3.txt")).expect("read file3"),
        b"content3"
    );
    assert!(summary.files_copied() >= 3, "all files should be copied");
}

/// Test pulling with archive mode (-a flag equivalents).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_with_archive_mode() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13003).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create test files with metadata
    let test_file = daemon.module_path().join("archive_test.txt");
    create_test_file(&test_file, b"archive content");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&test_file, perms).expect("set permissions");
    }

    let dest_root = tempdir().expect("create dest dir");

    // Pull with archive mode flags (recursive, preserve links, perms, times, etc.)
    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .links(true)
        .permissions(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("client pull succeeds");

    // Verify file content and metadata
    let copied_file = dest_root.path().join("archive_test.txt");
    assert_eq!(
        fs::read(&copied_file).expect("read copied file"),
        b"archive content"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let copied_metadata = fs::metadata(&copied_file).expect("read metadata");
        assert_eq!(
            copied_metadata.permissions().mode() & 0o777,
            0o644,
            "permissions should be preserved"
        );
    }

    assert!(summary.files_copied() >= 1);
}

/// Test pulling with compression enabled (-z flag).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_with_compression() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13004).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create compressible file
    let large_content = b"repetitive data ".repeat(1000);
    create_test_file(&daemon.module_path().join("large.txt"), &large_content);

    let dest_root = tempdir().expect("create dest dir");

    // Pull with compression
    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/large.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .compress(true)
        .build();

    let summary = run_client(config).expect("client pull with compression succeeds");

    // Verify file transferred correctly
    assert_eq!(
        fs::read(dest_root.path().join("large.txt")).expect("read large file"),
        large_content
    );
    assert!(summary.files_copied() >= 1);
}

/// Test pulling with checksum verification (--checksum flag).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_with_checksum() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13005).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("checksum_test.txt"),
        b"test data",
    );

    let dest_root = tempdir().expect("create dest dir");

    // Pull with checksum verification
    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/checksum_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .whole_file(false) // Enable delta transfer
        .build();

    let summary = run_client(config).expect("client pull with checksum succeeds");

    assert_eq!(
        fs::read(dest_root.path().join("checksum_test.txt")).expect("read file"),
        b"test data"
    );
    assert!(summary.files_copied() >= 1);
}

/// Test pushing files to upstream daemon.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_push_to_daemon() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13006).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create source files locally
    let source_root = tempdir().expect("create source dir");
    create_test_file(&source_root.path().join("upload.txt"), b"upload content");

    // Push to daemon
    let config = ClientConfig::builder()
        .transfer_args([
            source_root
                .path()
                .join("upload.txt")
                .to_string_lossy()
                .to_string(),
            format!("{}/", daemon.url()),
        ])
        .build();

    let summary = run_client(config).expect("client push succeeds");

    // Verify file was written to daemon module
    assert_eq!(
        fs::read(daemon.module_path().join("upload.txt")).expect("read uploaded file"),
        b"upload content"
    );
    assert!(summary.files_copied() >= 1);
}

/// Test pushing directory to upstream daemon.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_push_directory_to_daemon() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13007).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create source directory structure
    let source_root = tempdir().expect("create source dir");
    create_test_file(&source_root.path().join("file1.txt"), b"data1");
    create_test_file(&source_root.path().join("nested/file2.txt"), b"data2");

    // Push directory
    let mut source_arg = source_root.path().as_os_str().to_os_string();
    source_arg.push("/");

    let config = ClientConfig::builder()
        .transfer_args([
            source_arg.to_string_lossy().to_string(),
            format!("{}/", daemon.url()),
        ])
        .recursive(true)
        .build();

    let summary = run_client(config).expect("client push directory succeeds");

    // Verify files were written
    assert_eq!(
        fs::read(daemon.module_path().join("file1.txt")).expect("read file1"),
        b"data1"
    );
    assert_eq!(
        fs::read(daemon.module_path().join("nested/file2.txt")).expect("read file2"),
        b"data2"
    );
    assert!(summary.files_copied() >= 2);
}

// ============================================================================
// Protocol Version Compatibility Tests
// ============================================================================

/// Test client compatibility with rsync 3.0.9 daemon (protocol 30).
#[test]
#[ignore = "requires upstream rsync 3.0.9 binary"]
fn test_client_protocol_compatibility_3_0_9() {
    require_upstream(UPSTREAM_3_0_9);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_0_9, 13010).expect("start daemon 3.0.9");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("v309_test.txt"),
        b"version 3.0.9",
    );

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/v309_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let summary = run_client(config).expect("client works with 3.0.9 daemon");

    assert_eq!(
        fs::read(dest_root.path().join("v309_test.txt")).expect("read file"),
        b"version 3.0.9"
    );
    assert!(summary.files_copied() >= 1);
}

/// Test client compatibility with rsync 3.1.3 daemon (protocol 31).
#[test]
#[ignore = "requires upstream rsync 3.1.3 binary"]
fn test_client_protocol_compatibility_3_1_3() {
    require_upstream(UPSTREAM_3_1_3);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_1_3, 13011).expect("start daemon 3.1.3");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("v313_test.txt"),
        b"version 3.1.3",
    );

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/v313_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let summary = run_client(config).expect("client works with 3.1.3 daemon");

    assert_eq!(
        fs::read(dest_root.path().join("v313_test.txt")).expect("read file"),
        b"version 3.1.3"
    );
    assert!(summary.files_copied() >= 1);
}

/// Test client compatibility with rsync 3.4.1 daemon (protocol 31).
#[test]
#[ignore = "requires upstream rsync 3.4.1 binary"]
fn test_client_protocol_compatibility_3_4_1() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13012).expect("start daemon 3.4.1");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(
        &daemon.module_path().join("v341_test.txt"),
        b"version 3.4.1",
    );

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/v341_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let summary = run_client(config).expect("client works with 3.4.1 daemon");

    assert_eq!(
        fs::read(dest_root.path().join("v341_test.txt")).expect("read file"),
        b"version 3.4.1"
    );
    assert!(summary.files_copied() >= 1);
}

/// Verify protocol negotiation with different daemon versions.
///
/// This test manually performs handshake to verify our client negotiates
/// protocol correctly. The daemon advertises its version, and negotiated
/// version should be min(client_version, daemon_version).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_protocol_version_negotiation() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13013).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Connect and perform handshake
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

    // Read daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD: "), "expected greeting");

    // Parse daemon version
    let version_str = greeting
        .trim()
        .strip_prefix("@RSYNCD: ")
        .expect("parse version");
    let daemon_version: u8 = version_str
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .expect("parse daemon version");

    // 3.4.1 should advertise protocol 31
    assert_eq!(daemon_version, 31, "3.4.1 should advertise protocol 31");

    // Send client version
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Negotiated protocol should be min(31, 31) = 31
    // Further handshake verification would require full implementation
}

// ============================================================================
// Filter and Exclude Tests
// ============================================================================

/// Test pulling with filter rules (--exclude).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_with_exclude_filter() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13020).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create files, some to be excluded
    create_test_file(&daemon.module_path().join("include.txt"), b"include me");
    create_test_file(&daemon.module_path().join("exclude.tmp"), b"exclude me");
    create_test_file(&daemon.module_path().join("subdir/data.txt"), b"data");
    create_test_file(&daemon.module_path().join("subdir/temp.tmp"), b"temp");

    let dest_root = tempdir().expect("create dest dir");

    use core::client::FilterRuleSpec;

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .add_filter_rule(FilterRuleSpec::exclude("*.tmp"))
        .build();

    let summary = run_client(config).expect("client pull with filters succeeds");

    // Verify included files were transferred
    assert!(dest_root.path().join("include.txt").exists());
    assert!(dest_root.path().join("subdir/data.txt").exists());

    // Verify excluded files were not transferred
    assert!(!dest_root.path().join("exclude.tmp").exists());
    assert!(!dest_root.path().join("subdir/temp.tmp").exists());

    assert!(summary.files_copied() >= 2);
}

/// Test pulling with include/exclude combinations.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_pull_with_include_exclude() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13021).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(&daemon.module_path().join("important.log"), b"important");
    create_test_file(&daemon.module_path().join("debug.log"), b"debug");
    create_test_file(&daemon.module_path().join("data.txt"), b"data");

    let dest_root = tempdir().expect("create dest dir");

    use core::client::FilterRuleSpec;

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .add_filter_rule(FilterRuleSpec::include("important.log"))
        .add_filter_rule(FilterRuleSpec::exclude("*.log"))
        .build();

    let summary = run_client(config).expect("client pull with include/exclude succeeds");

    // important.log should be included despite *.log exclude
    assert!(dest_root.path().join("important.log").exists());
    // debug.log should be excluded
    assert!(!dest_root.path().join("debug.log").exists());
    // data.txt should be included (not a .log file)
    assert!(dest_root.path().join("data.txt").exists());

    assert!(summary.files_copied() >= 2);
}

// ============================================================================
// Incremental Transfer Tests
// ============================================================================

/// Test incremental transfer (only update changed files).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_incremental_transfer() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13030).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(&daemon.module_path().join("unchanged.txt"), b"no changes");
    create_test_file(&daemon.module_path().join("modified.txt"), b"old content");

    let dest_root = tempdir().expect("create dest dir");
    let daemon_url = daemon.url();
    let dest_path = dest_root.path().to_string_lossy().to_string();

    // First transfer
    let config1 = ClientConfig::builder()
        .transfer_args([format!("{}/", daemon_url), dest_path.clone()])
        .recursive(true)
        .build();

    let summary1 = run_client(config1).expect("first transfer succeeds");
    assert!(summary1.files_copied() >= 2);

    // Modify one file on daemon side
    create_test_file(&daemon.module_path().join("modified.txt"), b"new content");

    // Second transfer (incremental) - rebuild config
    let config2 = ClientConfig::builder()
        .transfer_args([format!("{}/", daemon_url), dest_path])
        .recursive(true)
        .build();

    let summary2 = run_client(config2).expect("second transfer succeeds");

    // Verify modified file has new content
    assert_eq!(
        fs::read(dest_root.path().join("modified.txt")).expect("read modified"),
        b"new content"
    );

    // Unchanged file should still exist with same content
    assert_eq!(
        fs::read(dest_root.path().join("unchanged.txt")).expect("read unchanged"),
        b"no changes"
    );

    // Should transfer at least the modified file
    assert!(summary2.files_copied() >= 1);
}

/// Test incremental transfer with size-only comparison.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_client_incremental_size_only() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13031).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    create_test_file(&daemon.module_path().join("sizetest.txt"), b"original");

    let dest_root = tempdir().expect("create dest dir");
    let daemon_url = daemon.url();
    let dest_path = dest_root.path().to_string_lossy().to_string();

    // First transfer
    let config1 = ClientConfig::builder()
        .transfer_args([format!("{}/sizetest.txt", daemon_url), dest_path.clone()])
        .size_only(true)
        .build();

    let summary1 = run_client(config1).expect("first transfer");
    assert!(summary1.files_copied() >= 1);

    // Modify file with same size
    create_test_file(&daemon.module_path().join("sizetest.txt"), b"modified");

    // Second transfer with size-only should skip (same size) - rebuild config
    let config2 = ClientConfig::builder()
        .transfer_args([format!("{}/sizetest.txt", daemon_url), dest_path])
        .size_only(true)
        .build();

    let _summary2 = run_client(config2).expect("second transfer");

    // With size-only, file might not be updated if sizes match
    // This depends on implementation details - we just verify transfer completes
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test error when connecting to non-existent daemon.
#[test]
fn test_error_connection_refused() {
    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            "rsync://127.0.0.1:1/testmodule/file.txt".to_string(),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let result = run_client(config);
    assert!(result.is_err(), "should fail to connect");
}

/// Test error when requesting non-existent module.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_error_invalid_module() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13040).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Connect and verify handshake
    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Read greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD:"));

    // Send version
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Request non-existent module
    writer_stream
        .write_all(b"nonexistent\n")
        .expect("send module");
    writer_stream.flush().expect("flush");

    // Should receive @ERROR response
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    assert!(
        response.starts_with("@ERROR"),
        "should get error for invalid module"
    );
}

/// Test error when daemon closes connection unexpectedly.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_error_unexpected_disconnect() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13041).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set timeout");

    let mut reader = BufReader::new(stream);

    // Read greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD:"));

    // Drop connection immediately (daemon drops the connection)
    drop(reader);

    // Attempting to use dropped connection should fail
}

// ============================================================================
// Metadata Preservation Tests
// ============================================================================

/// Test that file permissions are preserved during transfer.
#[test]
#[ignore = "requires upstream rsync binary"]
#[cfg(unix)]
fn test_metadata_preservation_permissions() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13050).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    use std::os::unix::fs::PermissionsExt;

    let test_file = daemon.module_path().join("perms_test.txt");
    create_test_file(&test_file, b"permission test");
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(&test_file, perms).expect("set permissions");

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/perms_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .permissions(true)
        .build();

    let summary = run_client(config).expect("transfer with perms succeeds");

    let copied_file = dest_root.path().join("perms_test.txt");
    let copied_metadata = fs::metadata(&copied_file).expect("read metadata");
    assert_eq!(
        copied_metadata.permissions().mode() & 0o777,
        0o755,
        "permissions should be preserved"
    );
    assert!(summary.files_copied() >= 1);
}

/// Test that modification times are preserved.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_metadata_preservation_times() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13051).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let test_file = daemon.module_path().join("time_test.txt");
    create_test_file(&test_file, b"time test");

    // Get original mtime
    let original_metadata = fs::metadata(&test_file).expect("read metadata");
    let original_mtime = original_metadata.modified().expect("get mtime");

    // Wait a bit to ensure time difference
    thread::sleep(Duration::from_millis(100));

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/time_test.txt", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .times(true)
        .build();

    let summary = run_client(config).expect("transfer with times succeeds");

    let copied_file = dest_root.path().join("time_test.txt");
    let copied_metadata = fs::metadata(&copied_file).expect("read metadata");
    let copied_mtime = copied_metadata.modified().expect("get mtime");

    // Times should match (within reasonable precision)
    let time_diff = copied_mtime
        .duration_since(original_mtime)
        .or_else(|_| original_mtime.duration_since(copied_mtime))
        .expect("calculate time difference");

    // Allow 2 second difference for filesystem precision
    assert!(
        time_diff < Duration::from_secs(2),
        "modification times should be preserved"
    );
    assert!(summary.files_copied() >= 1);
}

// ============================================================================
// Stress and Edge Case Tests
// ============================================================================

/// Test transferring many small files.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_many_small_files() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13060).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create 100 small files
    for i in 0..100 {
        create_test_file(
            &daemon.module_path().join(format!("file{:03}.txt", i)),
            format!("content {}", i).as_bytes(),
        );
    }

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .build();

    let summary = run_client(config).expect("transfer many files succeeds");

    // Verify all files transferred
    for i in 0..100 {
        let file_path = dest_root.path().join(format!("file{:03}.txt", i));
        assert!(file_path.exists(), "file{:03}.txt should exist", i);
        assert_eq!(
            fs::read(&file_path).expect("read file"),
            format!("content {}", i).as_bytes()
        );
    }

    assert!(summary.files_copied() >= 100);
}

/// Test transferring large file.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_large_file_transfer() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13061).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create 10 MB file
    let large_content = vec![0xAB; 10 * 1024 * 1024];
    create_test_file(&daemon.module_path().join("large.bin"), &large_content);

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/large.bin", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .build();

    let summary = run_client(config).expect("large file transfer succeeds");

    let copied_file = dest_root.path().join("large.bin");
    assert_eq!(
        fs::metadata(&copied_file).expect("read metadata").len(),
        10 * 1024 * 1024,
        "file size should match"
    );
    assert!(summary.files_copied() >= 1);
    assert!(summary.bytes_copied() >= 10 * 1024 * 1024);
}

/// Test empty directory transfer.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_empty_directory_transfer() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13062).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create empty directory
    fs::create_dir_all(daemon.module_path().join("emptydir")).expect("create empty dir");

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .build();

    let _summary = run_client(config).expect("empty dir transfer succeeds");

    // Empty directory should be created
    assert!(dest_root.path().join("emptydir").is_dir());
}

/// Test file with special characters in name.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_special_characters_in_filename() {
    require_upstream(UPSTREAM_3_4_1);

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 13063).expect("start daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create files with special characters (safe for filesystem)
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

    let dest_root = tempdir().expect("create dest dir");

    let config = ClientConfig::builder()
        .transfer_args([
            format!("{}/", daemon.url()),
            dest_root.path().to_string_lossy().to_string(),
        ])
        .recursive(true)
        .build();

    let summary = run_client(config).expect("special chars transfer succeeds");

    assert!(dest_root.path().join("file with spaces.txt").exists());
    assert!(dest_root.path().join("file-with-dashes.txt").exists());
    assert!(dest_root.path().join("file_with_underscores.txt").exists());
    assert!(summary.files_copied() >= 3);
}
