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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// Default timeout for daemon readiness checks.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval for TCP readiness probe.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Upstream rsync binary paths (relative to workspace root).
#[allow(dead_code)]
pub const UPSTREAM_3_0_9: &str = "target/interop/upstream-install/3.0.9/bin/rsync";
#[allow(dead_code)]
pub const UPSTREAM_3_1_3: &str = "target/interop/upstream-install/3.1.3/bin/rsync";
#[allow(dead_code)]
pub const UPSTREAM_3_4_1: &str = "target/interop/upstream-install/3.4.1/bin/rsync";

/// Selects which daemon binary to launch.
#[allow(dead_code)]
pub enum DaemonBinary {
    /// Upstream rsync binary at a specific path.
    Upstream(&'static str),
    /// The `oc-rsync` binary built by the current Cargo invocation.
    OcRsync,
}

impl DaemonBinary {
    /// Resolve to an actual binary path.
    ///
    /// The oc-rsync arm goes through [`test_support::oc_rsync_bin`], which
    /// knows the profile directory Cargo is building into. The previous
    /// `target/{release,debug}/oc-rsync` probe was both cwd-relative and
    /// profile-guessing, so it could launch a daemon from a different
    /// revision than the one under test.
    fn resolve(&self) -> io::Result<PathBuf> {
        match self {
            DaemonBinary::Upstream(path) => {
                if !Path::new(path).exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("upstream rsync binary not found at: {path}"),
                    ));
                }
                Ok(PathBuf::from(path))
            }
            DaemonBinary::OcRsync => Ok(test_support::oc_rsync_bin()),
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
                let mut error_msg = format!("daemon exited immediately with status: {status}");
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
    /// Polls with exponential backoff starting at `READY_POLL_INTERVAL`.
    /// Returns error if timeout is reached before daemon is ready.
    fn wait_ready(&self, timeout: Duration) -> io::Result<()> {
        let start = Instant::now();
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
                Err(_) => thread::sleep(READY_POLL_INTERVAL),
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

/// Candidate locations for a real upstream rsync, most specific first.
///
/// The interop harness installs versioned builds under `target/`; a developer
/// machine usually only has the packaged binary. Both are acceptable as long
/// as the binary is genuinely upstream, which [`upstream_rsync`] verifies from
/// `--version` rather than from the path it was found at.
#[allow(dead_code)]
const UPSTREAM_CANDIDATES: &[&str] = &[
    UPSTREAM_3_4_1,
    "/opt/homebrew/bin/rsync",
    "/usr/local/bin/rsync",
    "/usr/bin/rsync",
    "/bin/rsync",
];

/// Locates an upstream rsync binary, or panics naming every path tried.
///
/// A missing upstream binary must fail the test rather than silently return:
/// a cross-implementation assertion that quietly asserts nothing is worse than
/// no test at all. Verifying the `--version` banner also rules out an
/// `oc-rsync` shim earlier on `PATH` masquerading as upstream.
#[allow(dead_code)]
pub fn upstream_rsync() -> PathBuf {
    for candidate in UPSTREAM_CANDIDATES {
        let path = Path::new(candidate);
        if !path.exists() {
            continue;
        }
        let Ok(output) = Command::new(path).arg("--version").output() else {
            continue;
        };
        let banner = String::from_utf8_lossy(&output.stdout);
        let first = banner.lines().next().unwrap_or_default();
        if first.starts_with("rsync  version") && first.contains("protocol version") {
            return path.to_path_buf();
        }
    }
    panic!(
        "no upstream rsync binary found; tried: {}. Build one with \
         `bash tools/ci/run_interop.sh` or install the packaged rsync.",
        UPSTREAM_CANDIDATES.join(", ")
    );
}

/// A TCP proxy that forwards a bounded number of bytes from the daemon to the
/// client and then stalls forever with both sockets still open.
///
/// This is what makes a mid-transfer interrupt test deterministic. Throttling
/// the client with `--bwlimit` does not work: upstream only throttles socket
/// *writes* (`io.c:861`), so on a daemon pull the limit has to be applied by
/// the daemon and is inert client-side. Capping the bytes instead means the
/// transfer is structurally unable to complete, so the kill can be gated on
/// observed on-disk progress rather than on a timing guess.
#[allow(dead_code)]
pub struct CappingProxy {
    port: u16,
    forwarded: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl CappingProxy {
    /// Starts a proxy in front of `target_port` that forwards at most `cap`
    /// bytes downstream.
    pub fn start(target_port: u16, cap: u64) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;

        let forwarded = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let accept_forwarded = Arc::clone(&forwarded);
        let accept_stop = Arc::clone(&stop);

        thread::spawn(move || {
            while !accept_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let Ok(daemon) = TcpStream::connect(("127.0.0.1", target_port)) else {
                            continue;
                        };
                        let _ = client.set_nonblocking(false);
                        let (Ok(client_rx), Ok(daemon_rx)) =
                            (client.try_clone(), daemon.try_clone())
                        else {
                            continue;
                        };
                        let up_stop = Arc::clone(&accept_stop);
                        thread::spawn(move || pump(client_rx, daemon, u64::MAX, None, up_stop));
                        let down_counter = Arc::clone(&accept_forwarded);
                        let down_stop = Arc::clone(&accept_stop);
                        thread::spawn(move || {
                            pump(daemon_rx, client, cap, Some(down_counter), down_stop);
                        });
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });

        Ok(Self {
            port,
            forwarded,
            stop,
        })
    }

    /// The port clients should connect to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Bytes forwarded daemon -> client so far.
    pub fn forwarded(&self) -> u64 {
        self.forwarded.load(Ordering::Relaxed)
    }
}

impl Drop for CappingProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Copies `src` into `dst` until `cap` bytes have moved, then parks with both
/// sockets open so the peer blocks instead of seeing EOF.
fn pump(
    mut src: TcpStream,
    mut dst: TcpStream,
    cap: u64,
    counter: Option<Arc<AtomicU64>>,
    stop: Arc<AtomicBool>,
) {
    use std::io::Write;

    let mut moved: u64 = 0;
    let mut buf = [0u8; 65536];
    while moved < cap {
        let want = ((cap - moved) as usize).min(buf.len());
        let read = match src.read(&mut buf[..want]) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if dst.write_all(&buf[..read]).is_err() {
            return;
        }
        moved += read as u64;
        if let Some(ref counter) = counter {
            counter.fetch_add(read as u64, Ordering::Relaxed);
        }
    }
    // Cap reached: hold both sockets open and forward nothing more.
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(25));
    }
}

/// Waits until some file under `root` has a non-zero length.
///
/// Gates the interrupt on real progress instead of a sleep, so the test cannot
/// kill the client before the receiver has opened and written its temp file.
#[allow(dead_code)]
pub fn wait_for_progress(root: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if tree_has_data(root) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn tree_has_data(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if tree_has_data(&path) {
                return true;
            }
        } else if entry.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
            return true;
        }
    }
    false
}

/// Background readers for a child's piped stdout/stderr.
///
/// A child whose pipes nobody reads blocks once the pipe buffer fills, which
/// would deadlock against the very interrupt the test is measuring.
#[allow(dead_code)]
pub struct PipeDrain {
    stdout: thread::JoinHandle<String>,
    stderr: thread::JoinHandle<String>,
}

#[allow(dead_code)]
impl PipeDrain {
    /// Takes the child's pipes and starts draining them.
    pub fn start(child: &mut Child) -> Self {
        let mut out = child.stdout.take().expect("stdout piped");
        let mut err = child.stderr.take().expect("stderr piped");
        Self {
            stdout: thread::spawn(move || {
                let mut buf = String::new();
                let _ = out.read_to_string(&mut buf);
                buf
            }),
            stderr: thread::spawn(move || {
                let mut buf = String::new();
                let _ = err.read_to_string(&mut buf);
                buf
            }),
        }
    }

    /// Joins both readers, returning `(stdout, stderr)`.
    pub fn join(self) -> (String, String) {
        (
            self.stdout.join().unwrap_or_default(),
            self.stderr.join().unwrap_or_default(),
        )
    }
}
