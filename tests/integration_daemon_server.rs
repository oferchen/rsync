//! Integration tests for rsync daemon server functionality.
//!
//! These tests start a local daemon in a thread and exercise the protocol
//! via TCP socket connections. They follow the patterns established in
//! `crates/daemon/src/tests/`.
//!
//! Test categories:
//! 1. Connection and greeting
//! 2. Module listing
//! 3. Protocol version negotiation
//! 4. Authentication flows
//! 5. Error handling (module not found, access denied)
//! 6. Max connections enforcement

mod integration;

use daemon::{DaemonConfig, run_daemon};
#[allow(unused_imports)]
use integration::helpers::*;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Global mutex for environment variable isolation between tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Environment variable names for fallback control.
const DAEMON_FALLBACK_ENV: &str = "OC_RSYNC_DAEMON_FALLBACK";
const CLIENT_FALLBACK_ENV: &str = "OC_RSYNC_FALLBACK";

/// Scoped helper that applies an environment change and restores the previous
/// value when dropped.
struct EnvGuard {
    key: String,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: This is for test isolation
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(ref value) = self.previous {
            // SAFETY: Restoring previous value
            unsafe {
                std::env::set_var(&self.key, value);
            }
        } else {
            // SAFETY: Removing variable
            unsafe {
                std::env::remove_var(&self.key);
            }
        }
    }
}

/// Global port counter for test isolation.
static TEST_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);

/// Allocate a unique test port.
fn allocate_test_port() -> u16 {
    // Use a base port that incorporates the process ID for better isolation
    let pid = std::process::id();
    let base = 30000 + ((pid % 1000) * 20) as u16;

    loop {
        let offset = TEST_PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        if offset > 15 {
            TEST_PORT_COUNTER.store(0, Ordering::SeqCst);
        }
        let port = base + (offset % 20);
        // Try to bind to verify port is available
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            drop(listener);
            return port;
        }
    }
}

/// Connect to daemon with retries.
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

// ============================================================================
// Module Listing Tests
// ============================================================================

#[test]
fn server_lists_modules_on_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs,Documentation"),
            OsString::from("--module"),
            OsString::from("logs=/var/log"),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected @RSYNCD greeting, got: {line}"
    );

    // Send list request (no version handshake needed for #list)
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    // Read capabilities
    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert!(line.contains("CAP"), "expected CAP line, got: {line}");

    // Read OK
    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert!(line.contains("OK"), "expected OK line, got: {line}");

    // Read modules
    let mut modules = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("module line");
        if line.contains("EXIT") {
            break;
        }
        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }

    // Verify modules
    assert!(
        modules.contains(&"docs".to_string()),
        "should list docs module"
    );
    assert!(
        modules.contains(&"logs".to_string()),
        "should list logs module"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should exit cleanly");
}

#[test]
fn server_lists_empty_when_no_modules() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // Read response lines until EXIT (may have CAP, OK, then EXIT)
    let mut modules = Vec::new();
    let mut got_exit = false;
    for _ in 0..10 {
        line.clear();
        if reader.read_line(&mut line).is_err() {
            break;
        }
        if line.is_empty() {
            break;
        }
        if line.contains("EXIT") {
            got_exit = true;
            break;
        }
        // Skip CAP and OK lines
        if line.contains("CAP") || line.contains("OK") {
            continue;
        }
        // Any other non-@ line is a module
        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }

    assert!(got_exit, "should eventually get EXIT");
    assert!(modules.is_empty(), "should have no modules: {modules:?}");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_filters_unlisted_modules() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();
    let temp = tempdir().expect("tempdir");

    // Create config with one unlisted module
    let config_path = temp.path().join("rsyncd.conf");
    let public_dir = temp.path().join("public");
    let private_dir = temp.path().join("private");
    fs::create_dir(&public_dir).expect("create public dir");
    fs::create_dir(&private_dir).expect("create private dir");

    let config_content = format!(
        "[public]\npath = {}\nuse chroot = false\n\n\
         [private]\npath = {}\nlist = false\nuse chroot = false\n",
        public_dir.display(),
        private_dir.display()
    );
    fs::write(&config_path, config_content).expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Read CAP and OK
    line.clear();
    reader.read_line(&mut line).expect("cap");
    line.clear();
    reader.read_line(&mut line).expect("ok");

    // Read modules until EXIT
    let mut modules = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("line");
        if line.contains("EXIT") {
            break;
        }
        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }

    // Only public should be listed
    assert!(
        modules.contains(&"public".to_string()),
        "public should be listed"
    );
    assert!(
        !modules.contains(&"private".to_string()),
        "private should NOT be listed"
    );

    drop(reader);
    let _ = handle.join();
}

// ============================================================================
// Protocol Version Tests
// ============================================================================

#[test]
fn server_sends_protocol_greeting_first() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream);

    // Should receive greeting without sending anything
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    assert!(
        line.starts_with("@RSYNCD:"),
        "server should send greeting first: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

#[test]
fn server_greeting_includes_version() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Parse version
    let after_prefix = line.strip_prefix("@RSYNCD: ").expect("prefix");
    let version_str = after_prefix.split_whitespace().next().expect("version");
    let parts: Vec<&str> = version_str.split('.').collect();

    assert_eq!(parts.len(), 2, "version should have major.minor format");
    assert!(parts[0].parse::<u32>().is_ok(), "major should be numeric");
    assert!(parts[1].parse::<u32>().is_ok(), "minor should be numeric");

    drop(reader);
    let _ = handle.join();
}

#[test]
fn server_greeting_includes_digests_for_protocol_31_plus() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // For protocol 31+, greeting should include digests
    let after_prefix = line.strip_prefix("@RSYNCD: ").expect("prefix").trim();
    let parts: Vec<&str> = after_prefix.split_whitespace().collect();

    let version_str = parts[0];
    let major: u32 = version_str
        .split('.')
        .next()
        .unwrap()
        .parse()
        .expect("major");

    if major >= 31 && parts.len() > 1 {
        // Should have at least one common digest
        let digests = &parts[1..];
        let has_common_digest = digests
            .iter()
            .any(|d| *d == "md4" || *d == "md5" || d.contains("sha") || d.contains("xxh"));
        assert!(
            has_common_digest,
            "protocol 31+ should advertise digests: {digests:?}"
        );
    }

    drop(reader);
    let _ = handle.join();
}

#[test]
fn server_accepts_older_protocol_version() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send older protocol version
    stream
        .write_all(b"@RSYNCD: 29.0\n")
        .expect("send old version");
    stream.flush().expect("flush");

    // Request list (should still work)
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    line.clear();
    reader.read_line(&mut line).expect("response");

    // Should get a valid response, not a protocol error
    assert!(
        line.starts_with("@RSYNCD:"),
        "should accept older protocol version: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn server_returns_error_for_unknown_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Request non-existent module
    stream
        .write_all(b"nonexistent_module_xyz\n")
        .expect("send module");
    stream.flush().expect("flush");

    // Should receive error
    line.clear();
    reader.read_line(&mut line).expect("error response");
    assert!(
        line.contains("@ERROR:"),
        "should get error for unknown module: {line}"
    );

    // Should receive EXIT
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert!(line.contains("EXIT"), "should get EXIT after error: {line}");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_returns_error_for_access_denied() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();
    let temp = tempdir().expect("tempdir");

    // Create config with hosts_allow that denies localhost
    let config_path = temp.path().join("rsyncd.conf");
    let module_dir = temp.path().join("restricted");
    fs::create_dir(&module_dir).expect("create module dir");

    let config_content = format!(
        "[restricted]\npath = {}\nhosts allow = 10.0.0.0/8\nuse chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_path, config_content).expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Request restricted module from localhost (should be denied)
    stream.write_all(b"restricted\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    let lower = line.to_lowercase();
    assert!(
        line.contains("@ERROR:") && lower.contains("access denied"),
        "should get access denied: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_sends_exit_after_error() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Request non-existent module
    stream.write_all(b"fake_module\n").expect("send module");
    stream.flush().expect("flush");

    // Read error
    line.clear();
    reader.read_line(&mut line).expect("error");
    assert!(line.contains("@ERROR:"));

    // Read EXIT
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(
        line.trim(),
        "@RSYNCD: EXIT",
        "expected EXIT after error: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_handles_empty_module_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Send empty line
    stream.write_all(b"\n").expect("send empty");
    stream.flush().expect("flush");

    // Should receive some response (error or daemon message)
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert!(
        line.contains("@ERROR:") || line.contains("@RSYNCD:"),
        "should get error or daemon response for empty: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_handles_early_disconnect() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    // Connect and immediately disconnect after greeting
    {
        let stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("greeting");
        // Drop connection
    }

    // Daemon should handle gracefully
    let result = handle.join().expect("daemon thread");
    // May be ok or error, but shouldn't panic
    let _ = result;
}

// ============================================================================
// Authentication Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn server_requests_auth_for_protected_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();
    let temp = tempdir().expect("tempdir");

    // Create secrets file
    let secrets_path = temp.path().join("secrets.txt");
    fs::write(&secrets_path, "testuser:testpassword\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600)).expect("chmod");

    // Create config with auth
    let config_path = temp.path().join("rsyncd.conf");
    let module_dir = temp.path().join("secure");
    fs::create_dir(&module_dir).expect("create module dir");

    let config_content = format!(
        "[secure]\npath = {}\nauth users = testuser\nsecrets file = {}\nuse chroot = false\n",
        module_dir.display(),
        secrets_path.display()
    );
    fs::write(&config_path, config_content).expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Request protected module
    stream.write_all(b"secure\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(
        line.contains("AUTHREQD"),
        "should get AUTHREQD challenge: {line}"
    );

    drop(reader);
    let _ = handle.join();
}

// ============================================================================
// Max Connections Tests
// ============================================================================

#[test]
fn server_enforces_max_connections() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();
    let temp = tempdir().expect("tempdir");

    // Create config with max connections = 1
    let config_path = temp.path().join("rsyncd.conf");
    let lock_dir = temp.path().join("locks");
    let module_dir = temp.path().join("limited");
    fs::create_dir(&lock_dir).expect("create lock dir");
    fs::create_dir(&module_dir).expect("create module dir");

    let config_content = format!(
        "lock file = {}/rsyncd.lock\n\n\
         [limited]\npath = {}\nmax connections = 1\nuse chroot = false\n",
        lock_dir.display(),
        module_dir.display()
    );
    fs::write(&config_path, config_content).expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    // First connection
    let mut stream1 = connect_with_retries(port);
    let mut reader1 = BufReader::new(stream1.try_clone().expect("clone"));

    let mut line = String::new();
    reader1.read_line(&mut line).expect("greeting1");

    stream1
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version1");
    stream1.flush().expect("flush1");

    stream1.write_all(b"limited\n").expect("send module1");
    stream1.flush().expect("flush module1");

    line.clear();
    reader1.read_line(&mut line).expect("response1");

    // First should get OK
    if line.contains("OK") {
        // Try second connection while first is active
        let mut stream2 = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect2");
        stream2.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut reader2 = BufReader::new(stream2.try_clone().expect("clone2"));

        line.clear();
        reader2.read_line(&mut line).expect("greeting2");

        stream2
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("send version2");
        stream2.flush().expect("flush2");

        stream2.write_all(b"limited\n").expect("send module2");
        stream2.flush().expect("flush module2");

        line.clear();
        reader2.read_line(&mut line).expect("response2");

        // Second should get max connections error
        let lower = line.to_lowercase();
        assert!(
            line.contains("@ERROR:") || lower.contains("max connections"),
            "second connection should be limited: {line}"
        );
    }

    drop(reader1);
    drop(stream1);
    let _ = handle.join();
}

// ============================================================================
// Module With Comments Test
// ============================================================================

#[test]
fn server_lists_modules_with_comments() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs,Documentation files"),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Read CAP and OK
    line.clear();
    reader.read_line(&mut line).expect("cap");
    line.clear();
    reader.read_line(&mut line).expect("ok");

    // Read module line
    line.clear();
    reader.read_line(&mut line).expect("module");

    // Module listing should include comment (tab-separated)
    if line.contains('\t') {
        let parts: Vec<&str> = line.trim().split('\t').collect();
        assert_eq!(parts[0], "docs", "module name should be 'docs'");
        assert_eq!(
            parts.get(1).copied(),
            Some("Documentation files"),
            "comment should be included"
        );
    }

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

// ============================================================================
// Invalid Input Handling Tests
// ============================================================================

#[test]
fn server_handles_invalid_greeting_response() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send garbage instead of proper version response
    stream
        .write_all(b"this is not valid\n")
        .expect("send garbage");
    stream.flush().expect("flush");

    // Daemon should handle gracefully
    line.clear();
    let result = reader.read_line(&mut line);

    if result.is_ok() && !line.is_empty() {
        // Should get error or treated as module name
        assert!(
            line.contains("@ERROR:") || line.contains("@RSYNCD:"),
            "should handle invalid input: {line}"
        );
    }

    drop(reader);
    let _ = handle.join();
}

#[test]
fn server_sanitizes_module_name_in_error() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    stream.write_all(b"@RSYNCD: 32.0\n").expect("send version");
    stream.flush().expect("flush");

    // Send module with control characters
    stream
        .write_all(b"module\x00with\x1bcontrol\n")
        .expect("send bad module");
    stream.flush().expect("flush");

    // Response should not contain raw control characters
    line.clear();
    reader.read_line(&mut line).expect("response");

    assert!(
        !line.contains('\x00') && !line.contains('\x1b'),
        "response should sanitize control characters: {line:?}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
