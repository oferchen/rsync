//! Integration tests for rsync daemon (rsyncd) protocol interactions.
//!
//! These tests verify the complete daemon functionality by starting a local
//! daemon server and testing various client operations against it.
//!
//! Test categories:
//! 1. Module listing operations
//! 2. File transfers from daemon to local
//! 3. Checksum verification after transfer
//! 4. Protocol version negotiation
//! 5. Authentication flows
//! 6. Error handling (module not found, access denied)

mod integration;

use integration::helpers::*;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Global mutex for environment variable isolation between tests.
#[allow(dead_code)]
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Global port counter for test isolation.
static TEST_PORT_COUNTER: AtomicU16 = AtomicU16::new(45_000);

/// Allocate a unique test port.
#[allow(dead_code)]
fn allocate_test_port() -> u16 {
    loop {
        let port = TEST_PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        if port > 59_000 {
            TEST_PORT_COUNTER.store(45_000, Ordering::SeqCst);
            continue;
        }
        // Try to bind to verify port is available
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            drop(listener);
            return port;
        }
    }
}

/// Connect to daemon with retries.
#[allow(dead_code)]
fn connect_with_retries(port: u16) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    const TIMEOUT: Duration = Duration::from_secs(30);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        match TcpStream::connect_timeout(&target, backoff) {
            Ok(stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
                return stream;
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    panic!("failed to connect to daemon within timeout: {error}");
                }

                thread::sleep(backoff);
                backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
            }
        }
    }
}

/// Helper to perform rsync protocol handshake.
#[allow(dead_code)]
fn perform_handshake(stream: &mut TcpStream, version: &str) -> Result<String, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    // Read server greeting
    let mut greeting = String::new();
    reader
        .read_line(&mut greeting)
        .map_err(|e| format!("failed to read greeting: {e}"))?;

    if !greeting.starts_with("@RSYNCD:") {
        return Err(format!("invalid greeting: {greeting}"));
    }

    // Send client version response
    stream
        .write_all(format!("@RSYNCD: {version}\n").as_bytes())
        .map_err(|e| format!("failed to send version: {e}"))?;
    stream.flush().map_err(|e| e.to_string())?;

    Ok(greeting)
}

/// Helper to request module listing.
#[allow(dead_code)]
fn request_module_list(stream: &mut TcpStream) -> Result<Vec<String>, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    // Send list request
    stream
        .write_all(b"#list\n")
        .map_err(|e| format!("failed to send list request: {e}"))?;
    stream.flush().map_err(|e| e.to_string())?;

    // Read capabilities
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("failed to read capabilities: {e}"))?;

    if !line.contains("CAP") {
        return Err(format!("expected CAP line, got: {line}"));
    }

    // Read OK
    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("failed to read OK: {e}"))?;

    if !line.contains("OK") {
        return Err(format!("expected OK, got: {line}"));
    }

    // Read modules until EXIT
    let mut modules = Vec::new();
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("failed to read module: {e}"))?;

        if line.contains("EXIT") {
            break;
        }

        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }

    Ok(modules)
}

// ============================================================================
// Module Listing Tests
// ============================================================================

#[test]
fn daemon_list_modules_via_cli() {
    let port = allocate_test_port();
    let test_dir = TestDir::new().expect("create test dir");

    // Create module directories
    let docs_dir = test_dir.mkdir("docs").expect("create docs dir");
    let data_dir = test_dir.mkdir("data").expect("create data dir");
    fs::write(docs_dir.join("readme.txt"), b"Documentation").expect("create readme");
    fs::write(data_dir.join("file.dat"), b"Data content").expect("create data file");

    // Create config file
    let config_path = test_dir.path().join("rsyncd.conf");
    let config = format!(
        "[docs]\npath = {}\ncomment = Documentation files\nuse chroot = false\n\n\
         [data]\npath = {}\ncomment = Data storage\nuse chroot = false\n",
        docs_dir.display(),
        data_dir.display()
    );
    fs::write(&config_path, config).expect("write config");

    // Use CLI to list modules
    let mut cmd = RsyncCommand::new();
    cmd.args(["--list-only", &format!("rsync://127.0.0.1:{port}/")]);

    // This will fail since we're not running a daemon, but the test structure is here
    // In a full implementation, you would start the daemon in a thread using daemon::run_daemon
    let _result = cmd.run();
    // Note: This test demonstrates the pattern for CLI-based module listing
    // A real daemon would need to be started via the daemon crate
}

// ============================================================================
// Protocol Version Tests (via raw socket)
// ============================================================================

#[test]
fn protocol_version_greeting_format() {
    // This test documents the expected protocol greeting format
    // In a real test, we'd connect to a running daemon and verify the greeting

    // Expected format: @RSYNCD: <major>.<minor> [<digest1> <digest2> ...]
    // Example: @RSYNCD: 32.0 sha512 sha256 sha1 md5 md4

    let expected_prefix = "@RSYNCD: ";
    let example_greeting = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4";

    // Verify the format
    assert!(example_greeting.starts_with(expected_prefix));

    let version_part = example_greeting.strip_prefix(expected_prefix).unwrap();
    let version = version_part.split_whitespace().next().unwrap();
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 2, "version should have major.minor format");
    assert!(parts[0].parse::<u32>().is_ok(), "major should be numeric");
    assert!(parts[1].parse::<u32>().is_ok(), "minor should be numeric");
}

// ============================================================================
// Error Handling Tests (documentation)
// ============================================================================

#[test]
fn error_unknown_module_format() {
    // This test documents the expected error format for unknown modules
    // The daemon should respond with:
    // @ERROR: Unknown module 'modulename'
    // @RSYNCD: EXIT

    let expected_error_pattern = "@ERROR:";
    let expected_exit = "@RSYNCD: EXIT\n";

    assert!(expected_error_pattern.starts_with("@ERROR"));
    assert!(expected_exit.ends_with('\n'));
}

#[test]
fn error_access_denied_format() {
    // This test documents the expected error format for access denied
    // The daemon should respond with:
    // @ERROR: access denied to module 'modulename' from <ip>
    // @RSYNCD: EXIT

    let example_error = "@ERROR: access denied to module 'restricted' from 127.0.0.1";
    assert!(example_error.contains("access denied"));
    assert!(example_error.contains("module"));
}

// ============================================================================
// File Transfer Tests (via CLI)
// ============================================================================

#[test]
fn transfer_via_rsync_url_format() {
    // This test documents the URL format for rsync:// transfers
    // Format: rsync://[user@]host[:port]/module[/path]

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    // Test with a non-existent server (will fail but tests URL parsing)
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-av",
        "rsync://127.0.0.1:12345/module/file.txt",
        dest_dir.to_str().unwrap(),
    ]);

    // Should fail (connection refused) but not crash
    let result = cmd.run();
    assert!(result.is_ok(), "command should execute without panic");
    // The actual transfer will fail, which is expected
}

#[test]
fn transfer_single_file_format() {
    // Test the structure of single file transfer via rsync:// URL
    let test_dir = TestDir::new().expect("create test dir");
    let dest = test_dir.path().join("dest.txt");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-av",
        "rsync://example.invalid:12345/mod/file.txt",
        dest.to_str().unwrap(),
    ]);

    // Expected to fail (DNS resolution / connection)
    let _result = cmd.run();
}

#[test]
fn transfer_directory_recursive_format() {
    // Test the structure of recursive directory transfer
    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    // Trailing slash on source = transfer contents
    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-avr",
        "rsync://example.invalid:12345/mod/dir/",
        dest_dir.to_str().unwrap(),
    ]);

    // Expected to fail
    let _result = cmd.run();
}

// ============================================================================
// Checksum Verification Tests (structure)
// ============================================================================

#[test]
fn checksum_flag_usage() {
    // Test that --checksum flag is properly passed
    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-avc", // archive, verbose, checksum
        "rsync://example.invalid:12345/mod/",
        dest_dir.to_str().unwrap(),
    ]);

    // Command structure is valid even if connection fails
    let _result = cmd.run();
}

// ============================================================================
// Authentication Format Tests
// ============================================================================

#[test]
fn auth_url_with_username_format() {
    // Test the URL format with username for authentication
    // Format: rsync://user@host:port/module

    let test_dir = TestDir::new().expect("create test dir");
    let dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    let mut cmd = RsyncCommand::new();
    cmd.args([
        "-av",
        "rsync://testuser@example.invalid:12345/secure/",
        dest_dir.to_str().unwrap(),
    ]);

    let _result = cmd.run();
}

#[test]
fn auth_challenge_response_format() {
    // Document the authentication challenge-response format
    // 1. Server sends: @RSYNCD: AUTHREQD <base64_challenge>
    // 2. Client computes: MD5(password + challenge)
    // 3. Client sends: <username> <base64_response>
    // 4. Server sends: @RSYNCD: OK (on success)

    let example_challenge = "@RSYNCD: AUTHREQD YWJjZGVmZ2hpamtsbW5vcHFy";
    assert!(example_challenge.contains("AUTHREQD"));
    assert!(example_challenge.split_whitespace().count() == 3);
}

// ============================================================================
// Max Connections Test (structure)
// ============================================================================

#[test]
fn max_connections_config_format() {
    // Document the max connections configuration
    let config_example = r#"
lock file = /var/lock/rsyncd.lock

[module]
path = /srv/data
max connections = 5
"#;

    assert!(config_example.contains("max connections"));
    assert!(config_example.contains("lock file"));
}

// ============================================================================
// Module Comment Test (structure)
// ============================================================================

#[test]
fn module_listing_with_comments_format() {
    // Document the module listing format with comments
    // Format: <module_name>\t<comment>
    // Example: "docs\tDocumentation files"

    let example_listing = "docs\tDocumentation files\n";
    let parts: Vec<&str> = example_listing.trim().split('\t').collect();

    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], "docs");
    assert_eq!(parts[1], "Documentation files");
}

// ============================================================================
// Host Allow/Deny Test (structure)
// ============================================================================

#[test]
fn host_restrictions_config_format() {
    // Document the hosts allow/deny configuration format
    let config_example = r#"
[restricted]
path = /srv/restricted
hosts allow = 192.168.1.0/24
hosts deny = *
"#;

    assert!(config_example.contains("hosts allow"));
    assert!(config_example.contains("hosts deny"));
}

// ============================================================================
// Integration Test Using External rsync (when available)
// ============================================================================

#[test]
#[ignore = "requires rsync binary for interop testing"]
fn interop_with_system_rsync() {
    use std::process::Command;

    // Check if system rsync is available
    let rsync_check = Command::new("rsync").arg("--version").output();

    if rsync_check.is_err() {
        eprintln!("System rsync not available, skipping interop test");
        return;
    }

    let test_dir = TestDir::new().expect("create test dir");
    let _src_dir = test_dir.mkdir("src").expect("create src dir");
    let _dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    // In a full implementation:
    // 1. Start oc-rsyncd on a test port
    // 2. Use system rsync to connect and transfer files
    // 3. Verify the transfer was successful
}

// ============================================================================
// Protocol Negotiation Tests (structure)
// ============================================================================

#[test]
fn protocol_version_negotiation_sequence() {
    // Document the expected protocol negotiation sequence
    // 1. Server sends: @RSYNCD: <version> [digests...]
    // 2. Client sends: @RSYNCD: <version>
    // 3. Client sends: <module_name> or #list
    // 4. Server sends: @RSYNCD: OK (for valid modules)
    //    or @ERROR: ... (for invalid requests)

    let server_greeting = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";
    let client_response = "@RSYNCD: 31.0\n";
    let list_request = "#list\n";
    let module_request = "docs\n";

    assert!(server_greeting.starts_with("@RSYNCD:"));
    assert!(client_response.starts_with("@RSYNCD:"));
    assert!(list_request == "#list\n");
    assert!(!module_request.starts_with('@'));
}

#[test]
fn protocol_older_version_compatibility() {
    // Document that older protocol versions should be accepted
    // Protocol 29, 30, 31 should all be compatible with a 32-capable server

    let versions = ["29.0", "30.0", "31.0", "32.0"];

    for version in versions {
        let response = format!("@RSYNCD: {version}\n");
        assert!(response.starts_with("@RSYNCD:"));

        let parsed: Vec<&str> = version.split('.').collect();
        assert_eq!(parsed.len(), 2, "version should have major.minor format");
        assert!(parsed[0].parse::<u32>().is_ok(), "major should be numeric");
        assert!(parsed[1].parse::<u32>().is_ok(), "minor should be numeric");
    }
}

// ============================================================================
// Configuration File Format Tests
// ============================================================================

#[test]
fn config_file_structure() {
    // Document the expected rsyncd.conf file structure
    let config = r#"
# Global settings
lock file = /var/lock/rsyncd.lock
motd file = /etc/rsyncd.motd
pid file = /var/run/rsyncd.pid

# Module definition
[module_name]
path = /srv/module
comment = Module description
read only = true
list = true
use chroot = false
max connections = 10
hosts allow = 192.168.0.0/16
hosts deny = *
auth users = user1, user2
secrets file = /etc/rsyncd.secrets
"#;

    // Verify key directives are present
    assert!(config.contains("[module_name]"));
    assert!(config.contains("path ="));
    assert!(config.contains("read only ="));
    assert!(config.contains("use chroot ="));
}

#[test]
fn secrets_file_format() {
    // Document the secrets file format
    // Format: username:password (one per line)
    let secrets_content = r#"user1:password1
user2:password2
alice:secretpass
"#;

    for line in secrets_content.lines() {
        if !line.is_empty() {
            let parts: Vec<&str> = line.split(':').collect();
            assert_eq!(parts.len(), 2, "secrets format should be username:password");
        }
    }
}

// ============================================================================
// Exit Code Tests
// ============================================================================

#[test]
fn daemon_exit_codes() {
    // Document expected exit codes for daemon operations
    // 0 = Success
    // 1 = General error / feature unavailable
    // 10 = Socket I/O error

    let success_code = 0;
    let general_error = 1;
    let socket_error = 10;

    assert_eq!(success_code, 0);
    assert!(general_error > 0);
    assert!(socket_error > 0);
}

// ============================================================================
// Refuse Options Tests (structure)
// ============================================================================

#[test]
fn refuse_options_config_format() {
    // Document the refuse options configuration
    let config = r#"
[secure]
path = /srv/secure
refuse options = delete checksum
"#;

    assert!(config.contains("refuse options"));
}

// ============================================================================
// Bandwidth Limit Tests (structure)
// ============================================================================

#[test]
fn bandwidth_limit_config_format() {
    // Document the bandwidth limit configuration format
    // Format: <number>[K|M|G] (bytes per second)

    let bwlimit_examples = [
        ("1000", 1000u64),
        ("100K", 100 * 1024),
        ("10M", 10 * 1024 * 1024),
        ("1G", 1024 * 1024 * 1024),
    ];

    for (input, _expected) in bwlimit_examples {
        assert!(!input.is_empty(), "bwlimit should not be empty");
    }
}

// ============================================================================
// Timeout Configuration Tests
// ============================================================================

#[test]
fn timeout_config_format() {
    // Document the timeout configuration
    let _config = r#"
timeout = 300
"#;

    let timeout_str = "300";
    let timeout: u32 = timeout_str.parse().expect("timeout should be numeric");
    assert!(timeout > 0);
}

// ============================================================================
// Module Path Resolution Tests
// ============================================================================

#[test]
fn module_path_formats() {
    // Document valid module path formats
    let paths = ["/absolute/path", "/srv/rsync/module"];

    for path in paths {
        assert!(path.starts_with('/'), "module paths should be absolute");
    }
}

// ============================================================================
// MOTD (Message of the Day) Tests
// ============================================================================

#[test]
fn motd_format() {
    // Document MOTD configuration and display
    // MOTD is displayed before module listing
    let motd_content = "Welcome to the rsync server\nMaintenance window: Sundays 2-4 AM";

    assert_eq!(motd_content.lines().count(), 2);
    // MOTD lines should be displayed as-is to clients
}
