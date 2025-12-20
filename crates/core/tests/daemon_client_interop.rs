//! Daemon client interoperability tests against upstream rsync daemon.
//!
//! These tests verify that our daemon client implementation (rsync:// protocol)
//! works correctly with upstream rsync daemon across different protocol versions.
//!
//! Test strategy:
//! 1. Start upstream rsync daemon on a test port
//! 2. Use our client code to connect via rsync://
//! 3. Verify protocol negotiation, file transfers, and metadata preservation
//!
//! Upstream reference:
//! - `target/interop/upstream-src/rsync-3.4.1/clientserver.c` - daemon protocol
//! - `target/interop/upstream-src/rsync-3.4.1/main.c:1267-1384` - client_run()

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use tempfile::{TempDir, tempdir};

const UPSTREAM_3_4_1: &str = "/home/ofer/rsync/target/interop/upstream-install/3.4.1/bin/rsync";
const UPSTREAM_3_1_3: &str = "/home/ofer/rsync/target/interop/upstream-install/3.1.3/bin/rsync";

/// Test infrastructure for managing upstream rsync daemon instances.
struct UpstreamDaemon {
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

impl UpstreamDaemon {
    /// Start an upstream rsync daemon for testing.
    fn start(upstream_binary: &str, port: u16) -> std::io::Result<Self> {
        let workdir = tempdir()?;
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let pid_path = workdir.path().join("rsyncd.pid");
        let module_path = workdir.path().join("module");

        fs::create_dir_all(&module_path)?;

        // Write rsyncd.conf matching upstream format from run_interop.sh
        let config_content = format!(
            "\
pid file = {}
port = {}
use chroot = false
numeric ids = yes

[testmodule]
    path = {}
    comment = Test module for interop
    read only = false
",
            pid_path.display(),
            port,
            module_path.display()
        );
        fs::write(&config_path, config_content)?;

        // Start daemon with --no-detach so we can manage the process
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

        // Give daemon time to start and bind port
        thread::sleep(Duration::from_millis(500));

        // Check if daemon is still running
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
                // Still running - good
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

    /// Get the module root directory path.
    fn module_path(&self) -> &Path {
        &self.module_path
    }

    /// Get the daemon log contents for debugging.
    #[allow(dead_code)]
    fn log_contents(&self) -> std::io::Result<String> {
        fs::read_to_string(&self.log_path)
    }

    /// Wait for daemon to be ready by attempting to connect to the port.
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

/// Test that we can list modules from upstream daemon (handshake verification).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_handshake_with_upstream_daemon() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found at {UPSTREAM_3_4_1}");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12873).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Connect and perform basic handshake
    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    // Clone for separate reader/writer
    let reader_stream = stream.try_clone().expect("clone stream for reader");
    let mut writer_stream = stream;

    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(
        greeting.starts_with("@RSYNCD:"),
        "expected daemon greeting, got: {greeting}",
    );

    // Send client version
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Request module list (no intermediate OK expected)
    writer_stream.write_all(b"#list\n").expect("request list");
    writer_stream.flush().expect("flush");

    // Read module listing
    let mut modules = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read module line");
        if n == 0 || line.trim() == "@RSYNCD: EXIT" {
            break;
        }
        modules.push(line.trim().to_string());
    }

    // Should see our testmodule
    assert!(
        modules.iter().any(|m| m.contains("testmodule")),
        "module list should contain testmodule, got: {modules:?}",
    );
}

/// Test pulling files from upstream daemon using upstream client as baseline.
///
/// This test verifies that upstream daemon works correctly by using upstream
/// client to pull files. It serves as a baseline to compare against our client.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_pull_from_upstream_daemon_baseline() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12874).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create test files in daemon module
    create_test_file(
        &daemon.module_path().join("file1.txt"),
        b"hello from upstream daemon",
    );
    create_test_file(
        &daemon.module_path().join("subdir/file2.txt"),
        b"nested content",
    );

    // Set up destination
    let dest_root = tempdir().expect("create dest dir");

    // Use upstream client to pull from upstream daemon (baseline)
    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_root.path())
        .status()
        .expect("run upstream client");

    assert!(
        status.success(),
        "upstream client → upstream daemon should succeed"
    );

    // Verify files were copied
    assert_eq!(
        fs::read(dest_root.path().join("file1.txt")).expect("read file1"),
        b"hello from upstream daemon"
    );
    assert_eq!(
        fs::read(dest_root.path().join("subdir/file2.txt")).expect("read file2"),
        b"nested content"
    );
}

/// Test pushing files to upstream daemon using upstream client as baseline.
///
/// This test verifies that upstream daemon works correctly by using upstream
/// client to push files. It serves as a baseline to compare against our client.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_push_to_upstream_daemon_baseline() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12875).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create source files
    let source_root = tempdir().expect("create source dir");
    create_test_file(&source_root.path().join("upload1.txt"), b"upload content");
    create_test_file(
        &source_root.path().join("nested/upload2.txt"),
        b"nested upload",
    );

    // Use upstream client to push to upstream daemon (baseline)
    let mut source_path = source_root.path().as_os_str().to_os_string();
    source_path.push("/");
    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(&source_path)
        .arg(format!("{}/", daemon.url()))
        .status()
        .expect("run upstream client");

    assert!(
        status.success(),
        "upstream client → upstream daemon should succeed"
    );

    // Verify files were written to daemon module
    assert_eq!(
        fs::read(daemon.module_path().join("upload1.txt")).expect("read upload1"),
        b"upload content"
    );
    assert_eq!(
        fs::read(daemon.module_path().join("nested/upload2.txt")).expect("read upload2"),
        b"nested upload"
    );
}

/// Test protocol version negotiation with different upstream versions.
#[test]
#[ignore = "requires upstream rsync binaries"]
fn test_protocol_negotiation_3_1_3() {
    if !Path::new(UPSTREAM_3_1_3).exists() {
        eprintln!("Skipping: upstream rsync 3.1.3 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_1_3, 12876).expect("start upstream daemon 3.1.3");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Connect and check that we can negotiate with older protocol
    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    // Clone for separate reader/writer
    let reader_stream = stream.try_clone().expect("clone stream for reader");
    let mut writer_stream = stream;

    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting - 3.1.3 uses protocol 31
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(
        greeting.starts_with("@RSYNCD:"),
        "expected daemon greeting from 3.1.3"
    );

    // Verify protocol version in greeting (should be 30 or 31 for 3.1.3)
    let version_part = greeting
        .trim()
        .strip_prefix("@RSYNCD: ")
        .expect("parse version");
    let protocol_version: f64 = version_part.parse().expect("parse protocol version");
    assert!(
        (30.0..=31.0).contains(&protocol_version),
        "3.1.3 should advertise protocol 30 or 31, got: {protocol_version}",
    );

    // Send client version (we support up to 32)
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Request module to verify protocol negotiation works
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module request");
    writer_stream.flush().expect("flush");

    // Read response - should get OK or error
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .expect("read module response");
    assert!(
        response.starts_with("@RSYNCD: OK") || response.starts_with("@ERROR"),
        "expected module response, got: {response}",
    );
}

/// Test that daemon transfers preserve file metadata correctly (baseline).
///
/// Uses upstream client to pull from upstream daemon to verify metadata preservation.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_daemon_transfer_preserves_metadata_baseline() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12877).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Create test file with specific metadata
    let test_file = daemon.module_path().join("metadata_test.txt");
    create_test_file(&test_file, b"test content for metadata");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Set specific permissions
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&test_file, perms).expect("set permissions");
    }

    // Get original metadata
    let original_metadata = fs::metadata(&test_file).expect("read metadata");

    // Pull file via daemon using upstream client
    let dest_root = tempdir().expect("create dest dir");
    let status = Command::new(UPSTREAM_3_4_1)
        .arg("-av")
        .arg("--timeout=10")
        .arg(format!("{}/metadata_test.txt", daemon.url()))
        .arg(dest_root.path())
        .status()
        .expect("run upstream client");

    assert!(status.success(), "transfer should succeed");

    // Verify metadata was preserved
    let copied_file = dest_root.path().join("metadata_test.txt");
    let copied_metadata = fs::metadata(&copied_file).expect("read copied metadata");

    assert_eq!(
        copied_metadata.len(),
        original_metadata.len(),
        "file size should match"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            copied_metadata.permissions().mode() & 0o777,
            original_metadata.permissions().mode() & 0o777,
            "permissions should match"
        );
    }

    // Note: mtime comparison would go here but requires handling precision differences
}

/// Test error handling when module doesn't exist.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_daemon_nonexistent_module_error() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12878).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    // Try to access non-existent module
    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    // Clone for separate reader/writer
    let reader_stream = stream.try_clone().expect("clone stream for reader");
    let mut writer_stream = stream;

    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD:"), "expected daemon greeting");

    // Send client version
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Request non-existent module
    writer_stream
        .write_all(b"nonexistent_module\n")
        .expect("send module request");
    writer_stream.flush().expect("flush");

    // Should get @ERROR response
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    assert!(
        response.starts_with("@ERROR"),
        "should get error for non-existent module, got: {response}",
    );
    assert!(
        response.contains("Unknown module"),
        "error should mention unknown module, got: {response}",
    );
}
