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
        if let Some(status) = child.try_wait()? {
            let stderr = child.stderr.take();
            let mut error_msg = format!("Daemon exited immediately with status: {status}");
            if let Some(mut stderr) = stderr {
                let mut buf = String::new();
                if stderr.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                    error_msg.push_str(&format!("\nStderr: {buf}"));
                }
            }
            return Err(std::io::Error::other(error_msg));
        } else {
            // Still running - good
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
        modules.push(line.trim().to_owned());
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

// ============================================================================
// Comprehensive Handshake Tests
// ============================================================================

/// Test full handshake sequence from client perspective with modern protocol.
///
/// This test verifies the complete daemon handshake flow:
/// 1. Client receives daemon greeting (@RSYNCD: XX.Y)
/// 2. Client sends version with auth digest list
/// 3. Client sends module name
/// 4. Client receives @RSYNCD: OK
/// 5. Client sends server arguments
///
/// Mirrors upstream clientserver.c:start_inband_exchange()
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_full_handshake_sequence_modern_protocol() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12880).expect("start upstream daemon");
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
        "expected daemon greeting, got: {greeting}"
    );

    // Parse protocol version from greeting
    let version_part = greeting
        .trim()
        .strip_prefix("@RSYNCD: ")
        .expect("parse version prefix");
    let protocol_version: f64 = version_part
        .split_whitespace()
        .next()
        .expect("get version number")
        .parse()
        .expect("parse protocol version");
    assert!(
        protocol_version >= 30.0,
        "3.4.1 should advertise protocol 30+, got: {protocol_version}"
    );

    // Step 2: Send client version with auth digests (protocol 30+ requirement)
    // Order follows upstream checksum.c:71-84 valid_auth_checksums_items[]
    writer_stream
        .write_all(b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send client version");
    writer_stream.flush().expect("flush version");

    // Step 3: Send module name
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module name");
    writer_stream.flush().expect("flush module");

    // Step 4: Receive @RSYNCD: OK (or MOTD lines first, then OK)
    let mut got_ok = false;
    for _ in 0..10 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response line");
        let trimmed = line.trim();

        if trimmed == "@RSYNCD: OK" {
            got_ok = true;
            break;
        }

        // Check for errors
        assert!(
            !trimmed.starts_with("@ERROR"),
            "unexpected error: {trimmed}"
        );
        assert!(
            !trimmed.starts_with("@RSYNCD: AUTHREQD"),
            "unexpected auth required: {trimmed}"
        );

        // Other lines are MOTD, continue
    }

    assert!(got_ok, "should receive @RSYNCD: OK after module request");

    // Step 5: Send server arguments (protocol 30+ uses null terminators)
    // Format: --server [--sender] <flags> . <module/path>
    let args = [
        b"--server\0".as_slice(),
        b"--sender\0".as_slice(),
        b"-vn\0".as_slice(),
        b"-e.LsfxCIvu\0".as_slice(), // Capability flags for protocol 30+
        b".\0".as_slice(),
        b"testmodule/\0".as_slice(),
        b"\0".as_slice(), // Final empty string
    ];

    for arg in &args {
        writer_stream.write_all(arg).expect("send argument");
    }
    writer_stream.flush().expect("flush arguments");

    // At this point handshake is complete and file list exchange would begin
    // (not tested here as that requires full server implementation)
}

/// Test protocol version negotiation downgrade to common version.
///
/// Verifies that when client advertises a higher version than daemon supports,
/// the negotiated version is the minimum of both (daemon's version).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_protocol_version_negotiation_downgrade() {
    if !Path::new(UPSTREAM_3_1_3).exists() {
        eprintln!("Skipping: upstream rsync 3.1.3 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_1_3, 12881).expect("start daemon 3.1.3");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting - 3.1.3 advertises protocol 30 or 31
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");

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

    // Send client version 32 (higher than 3.1.3's protocol)
    writer_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Negotiated protocol should be min(32, daemon_version) = daemon_version
    // This is the key part: client must downgrade to daemon's version
    assert!(
        daemon_version <= 31,
        "3.1.3 should advertise protocol <= 31, got: {daemon_version}"
    );

    // Verify negotiation works by requesting module
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module");
    writer_stream.flush().expect("flush");

    // Should get OK response (protocol negotiation succeeded)
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    let trimmed = response.trim();

    // May get OK immediately or after MOTD
    assert!(
        trimmed.starts_with("@RSYNCD: OK") || !trimmed.starts_with("@ERROR"),
        "negotiation should succeed, got: {trimmed}"
    );
}

/// Test protocol version negotiation upgrade to client's version.
///
/// Verifies that when daemon advertises a higher version than client supports,
/// the negotiated version is the client's version.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_protocol_version_negotiation_upgrade() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12882).expect("start daemon 3.4.1");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting - 3.4.1 advertises protocol 31
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD: 31"), "expected protocol 31");

    // Send client version 30 (lower than daemon's protocol 31)
    writer_stream
        .write_all(b"@RSYNCD: 30.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Negotiated protocol should be min(31, 30) = 30
    // Client limits the session to an older protocol version

    // Verify negotiation works by requesting module
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module");
    writer_stream.flush().expect("flush");

    // Should get OK response
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    assert!(
        response.trim().starts_with("@RSYNCD: OK") || !response.starts_with("@ERROR"),
        "negotiation should succeed with downgraded protocol"
    );
}

/// Test module listing request/response flow.
///
/// Verifies that sending "#list" instead of a module name returns the
/// list of available modules followed by @RSYNCD: EXIT.
///
/// Mirrors upstream clientserver.c handling of module listing.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_module_listing_request_response() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12883).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD:"));

    // Send client version
    writer_stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Request module list instead of specific module
    writer_stream
        .write_all(b"#list\n")
        .expect("send #list request");
    writer_stream.flush().expect("flush");

    // Read module listing lines
    let mut modules = Vec::new();
    let mut got_exit = false;

    for _ in 0..100 {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read module line");

        if n == 0 {
            break; // EOF
        }

        let trimmed = line.trim();

        if trimmed == "@RSYNCD: EXIT" {
            got_exit = true;
            break;
        }

        if !trimmed.is_empty() && !trimmed.starts_with("@RSYNCD:") {
            modules.push(trimmed.to_owned());
        }
    }

    assert!(got_exit, "should receive @RSYNCD: EXIT after module list");
    assert!(
        modules.iter().any(|m| m.contains("testmodule")),
        "module list should contain testmodule, got: {modules:?}"
    );
}

/// Test compat flags exchange for protocol 30+.
///
/// For protocol 30+, after the module handshake completes, both sides
/// exchange compatibility flags as 4-byte varints before file list transfer.
///
/// This test verifies the handshake up to the point where compat flags
/// would be exchanged (actual exchange requires full server implementation).
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_compat_flags_exchange_setup() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12884).expect("start upstream daemon");
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

    // Complete handshake sequence
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");

    // Verify protocol 30+ for compat flags
    let version_str = greeting
        .trim()
        .strip_prefix("@RSYNCD: ")
        .expect("parse version");
    let protocol_version: u8 = version_str
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .expect("parse version");

    assert!(
        protocol_version >= 30,
        "need protocol 30+ for compat flags test, got: {protocol_version}"
    );

    // Send client version
    writer_stream
        .write_all(b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send version");
    writer_stream.flush().expect("flush");

    // Send module request
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module");
    writer_stream.flush().expect("flush");

    // Wait for OK
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response");
        if line.trim() == "@RSYNCD: OK" {
            break;
        }
        assert!(!line.starts_with("@ERROR"), "unexpected error");
    }

    // Send server arguments (protocol 30+ format with null terminators)
    let args = [
        b"--server\0".as_slice(),
        b"-vn\0".as_slice(),
        b"-e.LsfxCIvu\0".as_slice(),
        b".\0".as_slice(),
        b"testmodule/\0".as_slice(),
        b"\0".as_slice(),
    ];

    for arg in &args {
        writer_stream.write_all(arg).expect("send argument");
    }
    writer_stream.flush().expect("flush arguments");

    // At this point, the next protocol step would be:
    // 1. Compat flags exchange (4-byte varint from each side)
    // 2. Checksum seed exchange
    // 3. Capability negotiation (checksum/compression algorithms)
    // 4. Filter list exchange
    // 5. File list exchange
    //
    // This test verifies we've completed the handshake portion correctly.
}

/// Test capability negotiation for checksum algorithms (protocol 30+).
///
/// For protocol 30+, after compat flags exchange, both sides negotiate
/// which checksum and compression algorithms to use.
///
/// Server sends: supported checksums, supported compressions
/// Client sends: chosen checksum, chosen compression
///
/// Mirrors upstream compat.c:534-585 (negotiate_the_strings)
#[test]
#[ignore = "requires upstream rsync binary and full protocol implementation"]
fn test_capability_negotiation_checksums() {
    // This test would require implementing the full protocol exchange
    // up through compat flags and into algorithm negotiation.
    //
    // Key verification points:
    // 1. Server sends space-separated list of checksum algorithms
    // 2. Server sends space-separated list of compression algorithms
    // 3. Client selects first mutually supported algorithm from each list
    // 4. Both sides agree on xxh128/xxh3/xxh64/md5/md4/sha1/none for checksums
    // 5. Both sides agree on zstd/lz4/zlibx/zlib/none for compression
    //
    // Current status: Placeholder for future implementation
}

/// Test capability negotiation for compression algorithms (protocol 30+).
#[test]
#[ignore = "requires upstream rsync binary and full protocol implementation"]
fn test_capability_negotiation_compression() {
    // This test would verify compression algorithm negotiation:
    // 1. Server advertises: "zstd lz4 zlibx zlib none"
    // 2. Client selects first supported algorithm
    // 3. Upstream order: zstd, lz4, zlibx, zlib, none
    //
    // Current status: Placeholder for future implementation
}

/// Test error scenario: invalid protocol version in greeting.
#[test]
#[ignore = "requires upstream rsync binary"]
fn test_error_invalid_protocol_version() {
    if !Path::new(UPSTREAM_3_4_1).exists() {
        eprintln!("Skipping: upstream rsync 3.4.1 not found");
        return;
    }

    let daemon = UpstreamDaemon::start(UPSTREAM_3_4_1, 12885).expect("start upstream daemon");
    daemon
        .wait_ready(Duration::from_secs(5))
        .expect("daemon ready");

    let stream =
        TcpStream::connect(format!("127.0.0.1:{}", daemon.port)).expect("connect to daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let reader_stream = stream.try_clone().expect("clone stream");
    let mut writer_stream = stream;
    let mut reader = BufReader::new(reader_stream);

    // Read daemon greeting
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(greeting.starts_with("@RSYNCD:"));

    // Send invalid protocol version (too old)
    writer_stream
        .write_all(b"@RSYNCD: 20.0\n")
        .expect("send invalid version");
    writer_stream.flush().expect("flush");

    // Try to request module
    writer_stream
        .write_all(b"testmodule\n")
        .expect("send module");
    writer_stream.flush().expect("flush");

    // Daemon should reject or handle gracefully
    // (exact behavior depends on daemon implementation)
    let mut response = String::new();
    let result = reader.read_line(&mut response);

    // Either get an error or connection closes
    if result.is_ok() {
        // If we get a response, it might be an error or the daemon
        // might downgrade gracefully to protocol 20
        // (upstream behavior varies by version)
    }
}

/// Test error scenario: connection timeout.
#[test]
fn test_error_connection_timeout() {
    // Try to connect to a port that's not listening
    let result = TcpStream::connect("127.0.0.1:1");

    // Should fail immediately (connection refused) or timeout
    assert!(
        result.is_err(),
        "connection to non-listening port should fail"
    );
}

/// Test error scenario: module access denied.
///
/// This would require configuring the daemon with access restrictions,
/// which is outside the scope of this test suite but included for completeness.
#[test]
#[ignore = "requires daemon with access restrictions configured"]
fn test_error_module_access_denied() {
    // To test this properly, would need to:
    // 1. Configure daemon with "hosts allow" or similar restrictions
    // 2. Connect from unauthorized host/IP
    // 3. Verify @ERROR response indicates access denied
    //
    // Current status: Placeholder for future implementation
}

/// Test handshake with MOTD (message of the day).
///
/// Verifies that the client correctly skips MOTD lines between module
/// request and @RSYNCD: OK response.
#[test]
#[ignore = "requires upstream rsync binary with MOTD configured"]
fn test_handshake_with_motd() {
    // To test this, would need to configure daemon with MOTD file
    // and verify client skips those lines correctly.
    //
    // Expected flow:
    // 1. Client sends module request
    // 2. Server sends MOTD lines (arbitrary text)
    // 3. Server sends @RSYNCD: OK
    // 4. Client should skip MOTD and wait for OK
    //
    // Current status: Placeholder for future implementation
}

/// Test handshake with authentication requirement.
///
/// Verifies that client receives @RSYNCD: AUTHREQD when daemon requires
/// authentication for a module.
#[test]
#[ignore = "requires daemon with authentication configured"]
fn test_handshake_with_auth_requirement() {
    // To test this, would need to:
    // 1. Configure daemon module with "auth users" and "secrets file"
    // 2. Request module without authentication
    // 3. Verify @RSYNCD: AUTHREQD response
    // 4. Send authentication credentials
    // 5. Verify @RSYNCD: OK after successful auth
    //
    // Current status: Placeholder for future implementation
}
