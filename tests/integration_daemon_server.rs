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
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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

/// Allocate a unique test port by letting the OS assign an ephemeral port.
///
/// Binds to port 0 so the kernel picks a free port, reads the assigned port,
/// then drops the listener. This eliminates the TOCTOU race from the previous
/// approach (bind-check-drop-rebind) where another process could steal the
/// port between drop and daemon bind.
///
/// The residual race window (between our drop and daemon bind) is minimized
/// because ephemeral ports are not immediately recycled by the kernel.
/// Combined with nextest retries in `.config/nextest.toml`, this is robust
/// against CI port contention.
fn allocate_test_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).expect("bind to ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// The digest name list a protocol > 31 client must advertise.
///
/// Upstream's daemon table is written strongest-first and is never filtered by
/// protocol version, so a client offering all five leaves the choice to the
/// daemon's own preference order.
///
/// upstream: compat.c:868-871 - `negotiate_daemon_auth()` walks the client's
/// list; checksum.c `valid_auth_checksums_items[]` holds the names.
const CLIENT_DIGEST_LIST: &str = "sha512 sha256 sha1 md5 md4";

/// Builds the client half of the `@RSYNCD:` exchange for `protocol`.
///
/// The digest name list is attached only above protocol 31. That gate is not
/// cosmetic: `exchange_protocols()` refuses a greeting that omits the list with
/// `@ERROR: your client omitted the digest name list` and returns -1, so a bare
/// `@RSYNCD: 32.0` never reaches the module request and every assertion past it
/// tests the refusal instead of the feature under test. At or below protocol 31
/// upstream leaves `daemon_auth_choices` NULL on purpose and
/// `negotiate_daemon_auth()` substitutes `protocol_version >= 30 ? "md5" :
/// "md4"`, which is the legacy path the protocol-29/30 tests below exercise.
///
/// upstream: clientserver.c:229-241 - the `remote_protocol > 31` arm of
/// `exchange_protocols()`; compat.c:868-871 - the no-list substitution.
fn client_greeting(protocol: u32) -> String {
    if protocol > 31 {
        format!("@RSYNCD: {protocol}.0 {CLIENT_DIGEST_LIST}\n")
    } else {
        format!("@RSYNCD: {protocol}.0\n")
    }
}

/// Sends the client greeting for `protocol` and flushes it.
///
/// Mandatory before any module request or `#list`: `handle_daemon_connection()`
/// runs `exchange_protocols()` at clientserver.c:1534 and only then reads the
/// command line at :1538. A test that jumps straight to `#list` is answered
/// with a startup error, not a listing.
fn send_client_greeting(stream: &mut TcpStream, protocol: u32) {
    stream
        .write_all(client_greeting(protocol).as_bytes())
        .expect("send client greeting");
    stream.flush().expect("flush client greeting");
}

/// The argument that keeps a test-started daemon in the foreground.
///
/// Mandatory at every daemon site in this file. `RuntimeOptions::detach`
/// defaults to `cfg!(unix)`, so `run_daemon` reaches `become_daemon()` in the
/// accept loop; the parent of that fork is this test binary and it exits 0
/// (`platform::daemonize::become_daemon`). The harness records the resulting
/// clean exit as a pass, so every assertion in the file was unreachable.
///
/// The window is not the spawn itself - `thread::spawn` returns before the
/// daemon thread reaches the fork. It is the first blocking socket call:
/// `connect_with_retries` parks the test thread for exactly as long as the
/// daemon needs to detach. An unconditional `panic!` placed straight after the
/// spawn therefore still fires, while the same `panic!` placed one line later,
/// after the connect, never runs - all 25 tests reported PASSED with 14 such
/// panics in place.
///
/// upstream: clientserver.c:1758-1761 - `if (no_detach) create_pid_file(); else
/// become_daemon();` runs before the listener is set up, which is why
/// `--no-detach` is the daemon's own opt-out rather than a test-only knob.
fn no_detach() -> OsString {
    OsString::from("--no-detach")
}

/// Connect to daemon with retries.
///
/// Uses aggressive initial retries (10ms) ramping to 200ms, with a 60s
/// deadline. The generous timeout accommodates slow CI runners (especially
/// nightly builds under load).
fn connect_with_retries(port: u16) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(10);
    const MAX_BACKOFF: Duration = Duration::from_millis(200);
    const TIMEOUT: Duration = Duration::from_secs(60);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        match TcpStream::connect_timeout(&target, Duration::from_millis(500)) {
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
            no_detach(),
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

    // The version exchange is not optional, for `#list` either:
    // `handle_daemon_connection()` runs `exchange_protocols()` (clientserver.c:1534)
    // and only reads the command line afterwards (:1538).
    send_client_greeting(&mut stream, 32);
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    // Read modules. `send_listing()` writes one `"%-15s\t%s\n"` row per listed
    // module and then `@RSYNCD: EXIT`; there is no capability or OK line in the
    // listing at all, so the reads that used to expect them consumed the module
    // rows this test then failed to find.
    //
    // upstream: clientserver.c:1374-1386 - `send_listing()`.
    let mut modules = Vec::new();
    let mut got_exit = false;
    loop {
        line.clear();
        // A zero-length read is end-of-file. Without this arm the loop spins on
        // a closed socket instead of ending, which is how a listing that never
        // reached the terminator turned into a hang rather than a failure.
        if reader.read_line(&mut line).expect("module line") == 0 {
            break;
        }
        if line.contains("EXIT") {
            got_exit = true;
            break;
        }
        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }
    assert!(
        got_exit,
        "listing must end with @RSYNCD: EXIT, got {modules:?}"
    );

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    send_client_greeting(&mut stream, 32);
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // With no modules configured, `send_listing()` writes no rows at all and
    // the terminator is the very first line. Asserting that directly - rather
    // than scanning up to ten lines and tolerating whatever arrives - is what
    // makes "no modules" distinguishable from "some modules we failed to
    // parse": the previous loop skipped any line containing "OK", which
    // `@RSYNCD: OK` and a module named `ok` both satisfy.
    //
    // upstream: clientserver.c:1374-1386 - `send_listing()`.
    line.clear();
    reader.read_line(&mut line).expect("listing terminator");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@RSYNCD: EXIT",
        "an empty module set must list nothing before the terminator"
    );

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    send_client_greeting(&mut stream, 32);
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Read module rows until the terminator. There is no capability or OK line
    // to skip - `send_listing()` writes rows then `@RSYNCD: EXIT`
    // (clientserver.c:1374-1386) - and the two reads that used to consume them
    // swallowed the single `public` row plus the terminator, after which this
    // loop spun on end-of-file forever. `read_line` returning 0 is EOF, not a
    // blank line, so the terminator check alone could never end the loop.
    let mut modules = Vec::new();
    let mut got_exit = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).expect("listing line") == 0 {
            break;
        }
        if line.contains("EXIT") {
            got_exit = true;
            break;
        }
        let module_name = line.split('\t').next().unwrap_or(&line).trim().to_string();
        if !module_name.is_empty() && !module_name.starts_with('@') {
            modules.push(module_name);
        }
    }
    assert!(
        got_exit,
        "listing must end with @RSYNCD: EXIT, got {modules:?}"
    );

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
            no_detach(),
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
            no_detach(),
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
            no_detach(),
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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Protocol 29 is at or below the digest-list gate, so the greeting
    // deliberately carries no list - that is the path under test.
    send_client_greeting(&mut stream, 29);

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

    // Request non-existent module
    stream
        .write_all(b"nonexistent_module_xyz\n")
        .expect("send module");
    stream.flush().expect("flush");

    // Should receive the refusal, naming the module the client asked for. A
    // bare `contains("@ERROR:")` would also be satisfied by the digest-list
    // refusal that this fixture used to trip on before it sent a valid
    // greeting, so the payload is pinned rather than the prefix.
    //
    // upstream: clientserver.c:1570 - `@ERROR: Unknown module '%s'`.
    line.clear();
    reader.read_line(&mut line).expect("error response");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@ERROR: Unknown module 'nonexistent_module_xyz'",
        "unknown-module refusal must name the requested module"
    );

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

    // Request restricted module from localhost (should be denied)
    stream.write_all(b"restricted\n").expect("send module");
    stream.flush().expect("flush");

    // Should receive access denied
    line.clear();
    reader.read_line(&mut line).expect("response");
    // Pin the module name too: a refusal that named a different module, or a
    // generic error, would satisfy a bare "access denied" substring.
    //
    // upstream: clientserver.c:780 -
    // `@ERROR: access denied to %s from %s (%s)`.
    assert!(
        line.starts_with("@ERROR: access denied to restricted from "),
        "hosts-deny refusal must match clientserver.c:780, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn server_closes_connection_after_error() {
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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

    // Request non-existent module
    stream.write_all(b"fake_module\n").expect("send module");
    stream.flush().expect("flush");

    // Read error
    line.clear();
    reader.read_line(&mut line).expect("error");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@ERROR: Unknown module 'fake_module'",
        "unknown-module refusal must match clientserver.c:1570"
    );

    // Then EOF, not `@RSYNCD: EXIT`. That terminator has exactly one producer
    // upstream - `send_listing()` at clientserver.c:1385 - and the
    // unknown-module arm at :1567-1572 writes the error and returns -1, which
    // drops the connection. Asserting a zero-length read is the discriminating
    // check: a daemon that kept the session open for more commands after
    // refusing one would block here instead.
    line.clear();
    let trailing = reader.read_line(&mut line).expect("read after error");
    assert_eq!(
        trailing, 0,
        "daemon must close the connection after the refusal, got: {line}"
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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

    // Send empty line
    stream.write_all(b"\n").expect("send empty");
    stream.flush().expect("flush");

    // An empty command line is not an error - it is a listing request.
    // `if (!*line || strcmp(line, "#list") == 0)` sends the module listing and
    // returns, so with no modules configured the reply is the bare terminator.
    // The old assertion accepted `@ERROR:` *or* `@RSYNCD:`, which every line
    // the daemon can emit satisfies; it could not tell a listing from a refusal.
    //
    // upstream: clientserver.c:1554-1558.
    line.clear();
    reader.read_line(&mut line).expect("response");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@RSYNCD: EXIT",
        "an empty module request must be answered with a listing"
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
            no_detach(),
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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

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

    // Both halves of the socket must go before the join. The daemon session is
    // parked reading the credentials this test deliberately never sends, so a
    // still-open `stream` - `reader` holds only a dup of it - leaves the
    // `--once` daemon waiting and `join()` never returns.
    drop(reader);
    drop(stream);
    let _ = handle.join();
}

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    // First connection
    let mut stream1 = connect_with_retries(port);
    let mut reader1 = BufReader::new(stream1.try_clone().expect("clone"));

    let mut line = String::new();
    reader1.read_line(&mut line).expect("greeting1");

    send_client_greeting(&mut stream1, 32);

    stream1.write_all(b"limited\n").expect("send module1");
    stream1.flush().expect("flush module1");

    line.clear();
    reader1.read_line(&mut line).expect("response1");

    // The first client must be admitted, and unconditionally so. Guarding the
    // rest of this test with `if line.contains("OK")` made the cap assertion
    // optional: any outcome that was not an admission - including the
    // digest-list refusal this fixture used to provoke - skipped straight to
    // the join and reported a pass having probed nothing.
    assert!(
        line.contains("@RSYNCD: OK"),
        "first client must occupy the single connection slot, got: {line}"
    );

    // Second connection, while the first still holds the slot.
    let mut stream2 = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect2");
    stream2.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut reader2 = BufReader::new(stream2.try_clone().expect("clone2"));

    line.clear();
    reader2.read_line(&mut line).expect("greeting2");

    send_client_greeting(&mut stream2, 32);

    stream2.write_all(b"limited\n").expect("send module2");
    stream2.flush().expect("flush module2");

    line.clear();
    reader2.read_line(&mut line).expect("response2");

    // The exact refusal, not merely "some @ERROR". `max connections = 1` is in
    // the module config, so the payload carries that limit.
    //
    // upstream: clientserver.c:799 -
    // `@ERROR: max connections (%d) reached -- try again later`.
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@ERROR: max connections (1) reached -- try again later",
        "second client must be refused with the upstream cap payload"
    );

    drop(reader2);
    drop(stream2);
    drop(reader1);
    drop(stream1);
    let _ = handle.join();
}

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    send_client_greeting(&mut stream, 32);
    stream.write_all(b"#list\n").expect("send list");
    stream.flush().expect("flush");

    // Read the single module row. The two reads that used to precede this one
    // consumed a capability and an OK line the listing never contains
    // (clientserver.c:1374-1386), so this read landed on end-of-file and the
    // `if line.contains('\t')` guard below silently skipped every assertion -
    // the test went on passing while checking nothing at all.
    line.clear();
    reader.read_line(&mut line).expect("module row");
    let row = line.trim_end_matches(['\r', '\n']);
    let (name, comment) = row
        .split_once('\t')
        .unwrap_or_else(|| panic!("listing row must be tab-separated, got: {row:?}"));
    assert_eq!(name.trim_end(), "docs", "module name should be 'docs'");
    assert_eq!(
        comment, "Documentation files",
        "listing row must carry the module comment"
    );

    line.clear();
    reader.read_line(&mut line).expect("terminator");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@RSYNCD: EXIT",
        "listing must end with the terminator"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

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
            no_detach(),
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

    // A line that does not parse as `@RSYNCD: %d.%d` is refused with one exact
    // message. The previous form asserted nothing twice over: the whole check
    // sat behind `if result.is_ok() && !line.is_empty()`, so a dropped
    // connection skipped it, and the surviving disjunction accepted any line
    // the daemon is capable of writing.
    //
    // upstream: clientserver.c:209-215 - the `sscanf(...) < 1` arm of
    // `exchange_protocols()` writes `@ERROR: protocol startup error`.
    line.clear();
    reader.read_line(&mut line).expect("refusal");
    assert_eq!(
        line.trim_end_matches(['\r', '\n']),
        "@ERROR: protocol startup error",
        "an unparseable greeting must be refused with the upstream text"
    );

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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send version
    send_client_greeting(&mut stream, 32);

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

/// Helper: starts a daemon with an auth-protected module and returns
/// (port, tempdir, daemon thread handle). The secrets file contains
/// `testuser:testpassword`.
#[cfg(unix)]
fn start_auth_daemon() -> (
    u16,
    tempfile::TempDir,
    thread::JoinHandle<Result<(), daemon::DaemonError>>,
) {
    let port = allocate_test_port();
    let temp = tempdir().expect("tempdir");

    let secrets_path = temp.path().join("secrets.txt");
    fs::write(&secrets_path, "testuser:testpassword\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600)).expect("chmod");

    let config_path = temp.path().join("rsyncd.conf");
    let module_dir = temp.path().join("authmod");
    fs::create_dir(&module_dir).expect("create module dir");

    let config_content = format!(
        "[authmod]\npath = {}\nauth users = testuser\nsecrets file = {}\nuse chroot = false\n",
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
            no_detach(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));
    (port, temp, handle)
}

/// Performs a full auth handshake against the daemon at the given port,
/// using the specified client protocol version and digest algorithm.
/// Returns the final response line from the daemon (either `@RSYNCD: OK` or `@ERROR`).
#[cfg(unix)]
fn perform_auth_handshake(
    port: u16,
    client_protocol: u8,
    digest: daemon::auth::DaemonAuthDigest,
) -> String {
    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    // Read server greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected greeting, got: {line}"
    );

    // Advertise exactly the digest under test above protocol 31, so the
    // daemon's negotiated choice is the one this handshake then computes with.
    // Sending no list would leave the daemon on the legacy fallback
    // (compat.c:868-871) and the `digest` argument would only ever describe the
    // client's own arithmetic - which is how these cases could name a digest
    // they never actually negotiated.
    let version_line = if client_protocol > 31 {
        format!("@RSYNCD: {client_protocol}.0 {}\n", digest.name())
    } else {
        format!("@RSYNCD: {client_protocol}.0\n")
    };
    stream
        .write_all(version_line.as_bytes())
        .expect("send version");
    stream.flush().expect("flush version");

    // Request the auth-protected module
    stream.write_all(b"authmod\n").expect("send module name");
    stream.flush().expect("flush module name");

    // Read AUTHREQD challenge
    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    assert!(line.contains("AUTHREQD"), "expected AUTHREQD, got: {line}");

    // Extract challenge string: "@RSYNCD: AUTHREQD <challenge>\n"
    let challenge = line
        .trim()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("parse challenge")
        .to_owned();

    // Compute auth response with the specified digest
    let response = daemon::auth::compute_auth_response(b"testpassword", &challenge, digest);

    // Send credentials: "username response\n"
    let credentials = format!("testuser {response}\n");
    stream
        .write_all(credentials.as_bytes())
        .expect("send credentials");
    stream.flush().expect("flush credentials");

    // Read the result line
    line.clear();
    reader.read_line(&mut line).expect("auth result");
    line.trim().to_owned()
}

/// Tests that protocol 32 auth succeeds when using SHA-512 (strongest available digest).
#[cfg(unix)]
#[test]
fn auth_flow_protocol_32_sha512_succeeds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 32, daemon::auth::DaemonAuthDigest::Sha512);
    assert!(
        result.contains("OK"),
        "protocol 32 with SHA-512 should succeed: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 32 auth succeeds when using MD5.
#[cfg(unix)]
#[test]
fn auth_flow_protocol_32_md5_succeeds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 32, daemon::auth::DaemonAuthDigest::Md5);
    assert!(
        result.contains("OK"),
        "protocol 32 with MD5 should succeed: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 30 auth succeeds when using MD5
/// (the default digest for protocol 30).
#[cfg(unix)]
#[test]
fn auth_flow_protocol_30_md5_succeeds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 30, daemon::auth::DaemonAuthDigest::Md5);
    assert!(
        result.contains("OK"),
        "protocol 30 with MD5 should succeed: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 30 auth REJECTS MD4 responses.
/// upstream: compat.c:858 -- protocol_version >= 30 uses MD5, not MD4.
#[cfg(unix)]
#[test]
fn auth_flow_protocol_30_md4_rejected() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 30, daemon::auth::DaemonAuthDigest::Md4);
    assert!(
        result.contains("@ERROR") && result.to_lowercase().contains("auth failed"),
        "protocol 30 with MD4 should be rejected: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 29 auth succeeds when using MD4
/// (the only digest for protocol < 30).
#[cfg(unix)]
#[test]
fn auth_flow_protocol_29_md4_succeeds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    // `Md4Old`, not `Md4`. A protocol-29 client sends no digest list, so
    // `negotiate_daemon_auth()` takes the `md4` fallback with `md4_is_old = 1`
    // and then rewrites the negotiated item to `CSUM_MD4_OLD`
    // (compat.c:888-891), which `sum_init()` seeds with four zero bytes that an
    // explicitly negotiated `md4` never gets. Computing plain `Md4` here
    // produces a response the daemon is correct to reject.
    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 29, daemon::auth::DaemonAuthDigest::Md4Old);
    assert!(
        result.contains("OK"),
        "protocol 29 must authenticate with the seeded legacy MD4: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 29 auth REJECTS MD5 responses.
/// upstream: compat.c:858 -- protocol_version < 30 uses MD4, not MD5.
#[cfg(unix)]
#[test]
fn auth_flow_protocol_29_md5_rejected() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 29, daemon::auth::DaemonAuthDigest::Md5);
    assert!(
        result.contains("@ERROR") && result.to_lowercase().contains("auth failed"),
        "protocol 29 with MD5 should be rejected: {result}"
    );
    let _ = handle.join();
}

/// Tests that protocol 32 auth succeeds when using SHA-256.
#[cfg(unix)]
#[test]
fn auth_flow_protocol_32_sha256_succeeds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();
    let result = perform_auth_handshake(port, 32, daemon::auth::DaemonAuthDigest::Sha256);
    assert!(
        result.contains("OK"),
        "protocol 32 with SHA-256 should succeed: {result}"
    );
    let _ = handle.join();
}

/// Tests that a wrong password is rejected regardless of protocol version.
#[cfg(unix)]
#[test]
fn auth_flow_wrong_password_rejected() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, "0");
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, "0");

    let (port, _temp, handle) = start_auth_daemon();

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    send_client_greeting(&mut stream, 32);

    stream.write_all(b"authmod\n").expect("send module");
    stream.flush().expect("flush");

    line.clear();
    reader.read_line(&mut line).expect("auth challenge");
    let challenge = line
        .trim()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("parse challenge");

    // Compute response with WRONG password
    let response = daemon::auth::compute_auth_response(
        b"wrong_password",
        challenge,
        daemon::auth::DaemonAuthDigest::Sha512,
    );
    let credentials = format!("testuser {response}\n");
    stream
        .write_all(credentials.as_bytes())
        .expect("send credentials");
    stream.flush().expect("flush");

    line.clear();
    reader.read_line(&mut line).expect("auth result");
    assert!(
        line.contains("@ERROR"),
        "wrong password should be rejected: {line}"
    );

    drop(reader);
    let _ = handle.join();
}
