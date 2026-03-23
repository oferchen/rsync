//! Shared test infrastructure for daemon interoperability tests.
//!
//! Provides `TestDaemon` for managing rsync daemon instances with OS-assigned
//! ports and reliable readiness detection - eliminating hardcoded ports and
//! sleep-based synchronization that cause flaky test failures.
//!
//! Design patterns applied:
//! - **Builder Pattern**: `TestDaemonBuilder` for flexible daemon configuration
//! - **Strategy Pattern**: `DaemonBinary` enum selects upstream vs oc-rsync binary
//! - **RAII**: `Drop` impl ensures daemon cleanup even on test panics
//! - **DRY**: Shared across all 3 interop test suites

use std::fs;
use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// Default timeout for daemon readiness checks.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Initial polling interval for TCP readiness probe (exponential backoff).
const READY_POLL_INITIAL: Duration = Duration::from_millis(10);

/// Maximum polling interval for TCP readiness probe (exponential backoff cap).
const READY_POLL_MAX: Duration = Duration::from_millis(200);

/// Upstream rsync binary paths (relative to workspace root).
#[allow(dead_code)]
pub const UPSTREAM_3_0_9: &str = "target/interop/upstream-install/3.0.9/bin/rsync";
#[allow(dead_code)]
pub const UPSTREAM_3_1_3: &str = "target/interop/upstream-install/3.1.3/bin/rsync";
#[allow(dead_code)]
pub const UPSTREAM_3_4_1: &str = "target/interop/upstream-install/3.4.1/bin/rsync";

/// oc-rsync binary paths for daemon testing.
#[allow(dead_code)]
const OC_RSYNC_RELEASE: &str = "target/release/oc-rsync";
#[allow(dead_code)]
const OC_RSYNC_DEBUG: &str = "target/debug/oc-rsync";

/// Selects which daemon binary to launch.
#[allow(dead_code)]
pub enum DaemonBinary {
    /// Upstream rsync binary at a specific path.
    Upstream(&'static str),
    /// oc-rsync binary (auto-selects release or debug).
    OcRsync,
}

impl DaemonBinary {
    /// Resolve to an actual binary path.
    fn resolve(&self) -> io::Result<&str> {
        match self {
            DaemonBinary::Upstream(path) => {
                if !Path::new(path).exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("upstream rsync binary not found at: {path}"),
                    ));
                }
                Ok(path)
            }
            DaemonBinary::OcRsync => {
                if Path::new(OC_RSYNC_RELEASE).exists() {
                    Ok(OC_RSYNC_RELEASE)
                } else if Path::new(OC_RSYNC_DEBUG).exists() {
                    Ok(OC_RSYNC_DEBUG)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "oc-rsync binary not found at {OC_RSYNC_RELEASE} or {OC_RSYNC_DEBUG}",
                        ),
                    ))
                }
            }
        }
    }

    /// Whether this is an oc-rsync binary (port passed via CLI, not config).
    fn is_oc_rsync(&self) -> bool {
        matches!(self, DaemonBinary::OcRsync)
    }
}

/// Allocate a free TCP port using OS ephemeral port assignment.
///
/// Binds to port 0, reads the assigned port, then drops the listener.
/// The brief window between drop and daemon bind is acceptable for tests
/// since each test gets a unique port from the OS.
fn allocate_test_port() -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// A managed rsync daemon instance for testing.
///
/// Handles lifecycle (start, readiness, cleanup) with RAII semantics.
/// Uses OS-assigned ports to avoid conflicts when tests run in parallel.
pub struct TestDaemon {
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

impl TestDaemon {
    /// Start a daemon with the given binary, auto-allocating a free port.
    ///
    /// The daemon is verified ready (accepting TCP connections) before returning.
    /// Eliminates both hardcoded ports and sleep-based synchronization.
    #[allow(dead_code)]
    pub fn start(binary: DaemonBinary) -> io::Result<Self> {
        let port = allocate_test_port()?;
        Self::start_on_port(binary, port)
    }

    /// Start a daemon on a specific port (for tests that need port control).
    #[allow(dead_code)]
    pub fn start_on_port(binary: DaemonBinary, port: u16) -> io::Result<Self> {
        let binary_path = binary.resolve()?;
        let is_oc = binary.is_oc_rsync();

        let workdir = tempdir()?;
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let pid_path = workdir.path().join("rsyncd.pid");
        let module_path = workdir.path().join("module");

        fs::create_dir_all(&module_path)?;

        // Write daemon configuration.
        // Upstream rsync: port goes in config file.
        // oc-rsync: port passed via --port CLI flag.
        let config_content = if is_oc {
            format!(
                "\
pid file = {pid}

[testmodule]
    path = {path}
    comment = Test module for interop
    read only = false
    list = yes
    use chroot = false
    numeric ids = yes
",
                pid = pid_path.display(),
                path = module_path.display()
            )
        } else {
            format!(
                "\
pid file = {pid}
port = {port}
use chroot = false
numeric ids = yes

[testmodule]
    path = {path}
    comment = Test module for interop
    read only = false
",
                pid = pid_path.display(),
                path = module_path.display()
            )
        };
        fs::write(&config_path, config_content)?;

        // Build command with --no-detach for process management.
        let mut cmd = Command::new(binary_path);
        cmd.arg("--daemon")
            .arg("--config")
            .arg(&config_path)
            .arg("--no-detach")
            .arg("--log-file")
            .arg(&log_path);

        // oc-rsync takes port via CLI; upstream reads it from config.
        if is_oc {
            cmd.arg("--port").arg(port.to_string());
        }

        let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;

        // Verify daemon didn't exit immediately by polling for up to 500ms.
        // A single fixed sleep is flaky on slow CI runners - poll instead.
        let startup_deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Some(status) = child.try_wait()? {
                let stderr = child.stderr.take();
                let mut error_msg =
                    format!("daemon exited immediately with status: {status}");
                if let Some(mut stderr) = stderr {
                    let mut buf = String::new();
                    if stderr.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                        error_msg.push_str(&format!("\nStderr: {buf}"));
                    }
                }
                return Err(io::Error::other(error_msg));
            }
            if Instant::now() >= startup_deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let daemon = Self {
            _workdir: workdir,
            config_path,
            log_path,
            pid_path,
            module_path,
            port,
            process: Some(child),
        };

        // Wait for daemon to accept TCP connections before returning.
        daemon.wait_ready(DEFAULT_READY_TIMEOUT)?;

        Ok(daemon)
    }

    /// Get the rsync:// URL for this daemon's test module.
    #[allow(dead_code)]
    pub fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/testmodule", self.port)
    }

    /// Get the OS-assigned port this daemon is listening on.
    #[allow(dead_code)]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the module root directory for file operations.
    #[allow(dead_code)]
    pub fn module_path(&self) -> &Path {
        &self.module_path
    }

    /// Get the daemon log contents for debugging test failures.
    #[allow(dead_code)]
    pub fn log_contents(&self) -> io::Result<String> {
        fs::read_to_string(&self.log_path)
    }

    /// Wait for daemon to accept TCP connections.
    ///
    /// Polls with exponential backoff starting at `READY_POLL_INITIAL`,
    /// doubling each iteration up to `READY_POLL_MAX`. This reduces
    /// wasted time on fast startups while avoiding busy-spinning on slow CI.
    fn wait_ready(&self, timeout: Duration) -> io::Result<()> {
        let start = Instant::now();
        let mut delay = READY_POLL_INITIAL;
        loop {
            if start.elapsed() > timeout {
                let log = self
                    .log_contents()
                    .unwrap_or_else(|_| String::from("(log unavailable)"));
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "daemon on port {} did not become ready within {timeout:?}\nLog: {log}",
                        self.port
                    ),
                ));
            }

            match TcpStream::connect(format!("127.0.0.1:{}", self.port)) {
                Ok(_) => return Ok(()),
                Err(_) => {
                    thread::sleep(delay);
                    delay = (delay * 2).min(READY_POLL_MAX);
                }
            }
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Create a test file with specific content, creating parent directories as needed.
#[allow(dead_code)]
pub fn create_test_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write test file");
}

/// Panic if the given upstream binary does not exist.
#[allow(dead_code)]
pub fn require_upstream(binary_path: &str) {
    if !Path::new(binary_path).exists() {
        eprintln!("Skipping: upstream rsync binary not found at {binary_path}");
        panic!("upstream binary required for this test");
    }
}
