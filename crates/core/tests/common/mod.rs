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
#[cfg(unix)]
use std::process::ExitStatus;
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

/// Upstream rsync binary paths, anchored at the workspace root at compile
/// time. Cargo runs these tests with CWD `crates/core/`, so a path relative
/// to the CWD would never resolve and every consumer would silently skip.
#[allow(dead_code)]
pub const UPSTREAM_3_0_9: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/interop/upstream-install/3.0.9/bin/rsync"
);
#[allow(dead_code)]
pub const UPSTREAM_3_1_3: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/interop/upstream-install/3.1.3/bin/rsync"
);
#[allow(dead_code)]
pub const UPSTREAM_3_4_1: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/interop/upstream-install/3.4.1/bin/rsync"
);
#[allow(dead_code)]
pub const UPSTREAM_3_4_4: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/interop/upstream-install/3.4.4/bin/rsync"
);

/// Selects which daemon binary to launch.
#[derive(Clone, Copy)]
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
/// Binds to port 0, reads the assigned port, then drops the listener so the
/// daemon can bind it. This is a TOCTOU window: between the drop here and the
/// daemon's bind, another process can claim the port under parallel load. The
/// port is not reserved once returned - callers that need a guaranteed bind
/// (see [`TestDaemon::start`]) must retry on a fresh port when the daemon loses
/// the race.
fn allocate_test_port() -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Whether a [`TestDaemon::start_on_port`] failure was the daemon losing the
/// allocate-then-bind race for its port.
///
/// [`allocate_test_port`] hands back a port the OS just freed, so under
/// parallel load another process can grab it in the drop->bind window. When
/// that happens the daemon logs `Address already in use` and never accepts
/// connections, so the readiness probe times out; that timeout error embeds
/// the daemon log verbatim (`wait_ready`). Matching the message is how the
/// retry loop tells a losable port race - safe to retry on a fresh port - apart
/// from a genuine startup failure it must propagate rather than mask.
#[allow(dead_code)]
pub fn is_addr_in_use(err: &io::Error) -> bool {
    err.to_string()
        .to_ascii_lowercase()
        .contains("address already in use")
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
    ///
    /// [`allocate_test_port`] cannot reserve the port it returns, so under
    /// parallel load the daemon can lose the allocate-then-bind race (TOCTOU)
    /// and fail readiness with `Address already in use`. Retry on a fresh port
    /// up to a small bound; a failed attempt's child is already killed and
    /// reaped by [`start_on_port`]'s `Drop` before the error propagates here.
    /// Any non-bind-conflict error is propagated immediately so a real startup
    /// bug is never masked by the retry.
    #[allow(dead_code)]
    pub fn start(binary: DaemonBinary) -> io::Result<Self> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_err: Option<io::Error> = None;
        for _ in 0..MAX_ATTEMPTS {
            let port = allocate_test_port()?;
            match Self::start_on_port(binary, port) {
                Ok(daemon) => return Ok(daemon),
                Err(err) if is_addr_in_use(&err) => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "daemon failed to bind a free port after retries",
            )
        }))
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
/// The reference version is 3.4.4; the harness build and the source-tree build
/// come first so a machine with both uses the pinned one rather than whatever
/// the package manager shipped.
#[allow(dead_code)]
const UPSTREAM_CANDIDATES: &[&str] = &[
    UPSTREAM_3_4_4,
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/interop/upstream-src/rsync-3.4.4/rsync"
    ),
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
/// no test at all. The `--version` banner - not the path - decides, which is
/// what rejects macOS's `/usr/bin/rsync` (`openrsync: protocol version 29`)
/// and an `oc-rsync` shim earlier on `PATH` masquerading as upstream.
#[allow(dead_code)]
pub fn upstream_rsync() -> PathBuf {
    let mut rejected = Vec::new();
    for candidate in UPSTREAM_CANDIDATES {
        let path = Path::new(candidate);
        if !path.exists() {
            rejected.push(format!("{candidate} (absent)"));
            continue;
        }
        let Ok(output) = Command::new(path).arg("--version").output() else {
            rejected.push(format!("{candidate} (--version failed)"));
            continue;
        };
        let banner = String::from_utf8_lossy(&output.stdout);
        let first = banner.lines().next().unwrap_or_default();
        if first.starts_with("rsync  version") && first.contains("protocol version") {
            return path.to_path_buf();
        }
        rejected.push(format!("{candidate} (not upstream: {first:?})"));
    }
    panic!(
        "no upstream rsync binary found; tried: {}. Build one with \
         `bash tools/ci/run_interop.sh` or install the packaged rsync.",
        rejected.join(", ")
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

/// Per-file payload size for the mid-transfer kill fixtures.
///
/// Comfortably larger than [`FORWARD_CAP`], so the transfer is structurally
/// unable to finish before the proxy stalls.
#[cfg(unix)]
#[allow(dead_code)]
pub const TEST_FILE_SIZE: usize = 2 * 1024 * 1024;

/// Daemon bytes [`CappingProxy`] forwards before stalling forever.
///
/// Large enough that the receiver has certainly opened its temp file and
/// written literal data (which is what arms upstream's `cleanup_got_literal`
/// gate, `cleanup.c:159`), small enough that no file is anywhere near
/// complete.
#[cfg(unix)]
#[allow(dead_code)]
pub const FORWARD_CAP: u64 = 512 * 1024;

/// How long to wait for the receiver to put bytes on disk before signalling.
#[cfg(unix)]
#[allow(dead_code)]
pub const PROGRESS_WAIT: Duration = Duration::from_secs(20);

/// How long the client may take to exit after the signal.
///
/// Upstream exits in ~0.4 s, dominated by the deliberate `msleep(400)` in
/// `rsync.c:694`. Anything in seconds here means the signal was not observed
/// while the transfer was parked in a blocking transport read - which is the
/// whole behaviour these fixtures exist to pin.
#[cfg(unix)]
#[allow(dead_code)]
pub const EXIT_WAIT: Duration = Duration::from_secs(10);

/// rsync's exit code for SIGINT/SIGTERM/SIGHUP (`errcode.h` `RERR_SIGNAL`).
#[cfg(unix)]
#[allow(dead_code)]
pub const RERR_SIGNAL: i32 = 20;

/// Deterministic payload: a repeating byte pattern, so a retained partial can
/// be checked to be a genuine prefix of the source rather than merely the
/// right length.
#[cfg(unix)]
#[allow(dead_code)]
pub fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
}

/// Whether a filename matches rsync's temp pattern `.name.XXXXXX`.
#[cfg(unix)]
#[allow(dead_code)]
pub fn is_temp_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    match rest.rfind('.') {
        // The random suffix is exactly 6 alphanumeric characters.
        Some(last_dot) => {
            let suffix = &rest[last_dot + 1..];
            suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Recursively collects every path under `dir` that looks like an rsync temp.
#[cfg(unix)]
#[allow(dead_code)]
pub fn find_temp_files(dir: &Path) -> Vec<PathBuf> {
    let mut temps = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                temps.extend(find_temp_files(&path));
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if is_temp_file_name(name) {
                    temps.push(path);
                }
            }
        }
    }
    temps
}

/// An oc-rsync daemon, a stalling [`CappingProxy`] in front of it, and a
/// destination directory: everything a mid-transfer kill test needs.
///
/// The proxy is what makes the kill deterministic. `--bwlimit` cannot be used
/// to open the window because upstream throttles socket *writes*
/// (`io.c:861`), so on a daemon pull the limit belongs to the daemon and is
/// inert client-side. Capping the forwarded bytes instead means the transfer
/// cannot complete at all, so the signal can be gated on observed on-disk
/// progress rather than on a timing guess.
#[cfg(unix)]
pub struct StalledTransfer {
    daemon: TestDaemon,
    proxy: CappingProxy,
    dest: TempDir,
}

#[cfg(unix)]
#[allow(dead_code)]
impl StalledTransfer {
    /// Starts the daemon and the proxy. Add payloads with [`Self::add_file`].
    pub fn start() -> Self {
        let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");
        let proxy =
            CappingProxy::start(daemon.port(), FORWARD_CAP).expect("start byte-capping proxy");
        Self {
            daemon,
            proxy,
            dest: tempdir().expect("create dest dir"),
        }
    }

    /// Publishes `name` in the module and returns the bytes written, so a
    /// retained partial can be compared against the true source prefix.
    pub fn add_file(&self, name: &str, size: usize) -> Vec<u8> {
        let data = generate_test_data(size);
        create_test_file(&self.daemon.module_path().join(name), &data);
        data
    }

    /// The `rsync://` URL of the module, routed through the stalling proxy.
    pub fn module_url(&self) -> String {
        format!("rsync://127.0.0.1:{}/testmodule", self.proxy.port())
    }

    /// The destination directory the client writes into.
    pub fn dest_path(&self) -> &Path {
        self.dest.path()
    }

    /// Daemon log contents, for failure messages.
    pub fn daemon_log(&self) -> String {
        self.daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    }

    /// Runs `client` with `args` plus the destination, waits for on-disk
    /// progress, delivers `signal`, and asserts the client exits
    /// [`RERR_SIGNAL`].
    ///
    /// upstream: `rsync.c:684 sig_int()` records the signal and lets the
    /// normal I/O path shut the transfer down via
    /// `cleanup.c:_exit_cleanup(RERR_SIGNAL)`.
    pub fn interrupt(&self, client: &Path, args: &[&str], signal: libc::c_int) -> ExitStatus {
        let mut child = Command::new(client)
            .args(args)
            .arg("--timeout=60")
            .arg(self.dest_path().as_os_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn client");
        let drain = PipeDrain::start(&mut child);

        assert!(
            wait_for_progress(self.dest_path(), PROGRESS_WAIT),
            "client wrote nothing to {} within {PROGRESS_WAIT:?}; proxy forwarded {} bytes; \
             daemon log: {}",
            self.dest_path().display(),
            self.proxy.forwarded(),
            self.daemon_log()
        );

        let status = signal_and_wait(&mut child, signal);
        let (_stdout, stderr) = drain.join();
        assert!(
            self.proxy.forwarded() <= FORWARD_CAP,
            "proxy must not exceed its cap: forwarded {}",
            self.proxy.forwarded()
        );
        assert_eq!(
            status.code(),
            Some(RERR_SIGNAL),
            "interrupted transfer must exit {RERR_SIGNAL}; stderr: {stderr}"
        );
        // Give the retained rename a moment to land before the caller stats.
        thread::sleep(Duration::from_millis(200));
        status
    }
}

/// Delivers `signal` to `child` and waits for it, failing the test if the
/// child outlives [`EXIT_WAIT`].
///
/// The timeout is the load-bearing assertion: a client that sets its shutdown
/// flag but never acts on it stays parked in the transport read forever, and
/// this is what turns that into a red test instead of a hang.
#[cfg(unix)]
#[allow(dead_code)]
pub fn signal_and_wait(child: &mut Child, signal: libc::c_int) -> ExitStatus {
    let pid = child.id();
    // SAFETY: sending a signal to a child this process spawned and has not reaped.
    let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
    assert_eq!(ret, 0, "failed to send signal {signal} to child pid {pid}");

    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "client pid {pid} ignored signal {signal} for {EXIT_WAIT:?}: the shutdown \
                         flag is set but nothing on the network path acted on it"
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("error waiting for client pid {pid}: {e}"),
        }
    }
}
