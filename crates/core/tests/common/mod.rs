//! Shared test infrastructure for daemon interoperability tests.
//!
//! Provides `TestDaemon`, which serves an rsync daemon on a listener the
//! harness binds itself, so tests need no hardcoded ports, no readiness
//! polling and no wall-clock bound on daemon start-up.
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
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

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
pub const UPSTREAM_3_5_1: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/interop/upstream-install/3.5.1/bin/rsync"
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

/// The upstream binary at `path`, or `NotFound` when it is absent.
#[cfg(unix)]
fn upstream_binary(path: &str) -> io::Result<PathBuf> {
    if Path::new(path).exists() {
        Ok(PathBuf::from(path))
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("upstream rsync binary not found at: {path}"),
        ))
    }
}

/// A managed rsync daemon for tests, on a loopback listener the harness binds
/// on an OS-assigned port before any daemon runs.
///
/// A client connect therefore queues in the kernel backlog: there is no port
/// race, no readiness wait and no restart loop.
///
/// - oc-rsync runs as the daemon library in a forked process
///   (`test_support::forked_server`) that serves the pre-bound listener, so the
///   real accept loop and TCP session are under test and no binary is
///   executed.
/// - An upstream binary runs in inetd mode, once per accepted connection with
///   the socket as its stdin and stdout, because a binary cannot take a
///   pre-bound listener. upstream: clientserver.c:1748
///   `is_inetd_socket(STDIN_FILENO)`.
#[allow(dead_code)]
pub struct TestDaemon {
    // Dropped first, so the daemon and every session are killed and reaped
    // before the working directory they serve is removed.
    #[cfg(unix)]
    server: Server,
    workdir: TempDir,
    log_path: PathBuf,
    module_path: PathBuf,
    port: u16,
}

impl TestDaemon {
    /// Starts serving `binary` on a fresh loopback port.
    ///
    /// The oc-rsync daemon serves as soon as this returns. An upstream binary
    /// is started once first, and this returns when that warm-up session has
    /// sent its `@RSYNCD:` greeting: that proves the binary starts and leaves
    /// it in the page cache, so a test's own timeouts never absorb a cold
    /// exec.
    ///
    /// # Errors
    ///
    /// Returns the setup I/O error, or [`io::ErrorKind::UnexpectedEof`] with
    /// the daemon's stderr and log when an upstream warm-up session ends
    /// before its greeting, meaning the daemon died.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn start(binary: DaemonBinary) -> io::Result<Self> {
        let workdir = test_support::create_tempdir();
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let module_path = workdir.path().join("module");
        fs::create_dir_all(&module_path)?;
        fs::write(&config_path, daemon_config(&module_path))?;

        let (server, port) = match binary {
            DaemonBinary::OcRsync => {
                let daemon = test_support::oc_daemon::OcDaemon::builder(&config_path)
                    .arg("--log-file")
                    .arg(&log_path)
                    .spawn()?;
                let port = daemon.port();
                (Server::Forked(daemon), port)
            }
            DaemonBinary::Upstream(path) => {
                use test_support::inetd_server::{InetdCommand, InetdServer};
                let stderr = fs::File::options()
                    .create(true)
                    .append(true)
                    .open(workdir.path().join("daemon.stderr"))?;
                let command = InetdCommand::new(upstream_binary(path)?, stderr)
                    .arg("--config")
                    .arg(&config_path)
                    .arg("--log-file")
                    .arg(&log_path);
                let server = InetdServer::start(TcpListener::bind("127.0.0.1:0")?, command)?;
                let port = server.port();
                (Server::Inetd(server), port)
            }
        };
        let daemon = Self {
            server,
            workdir,
            log_path,
            module_path,
            port,
        };
        if matches!(daemon.server, Server::Inetd(_)) {
            daemon.await_greeting()?;
        }
        Ok(daemon)
    }

    /// The daemon cannot take an accepted socket as its stdio here; the oc
    /// daemon is Unix-only by design (`daemon.rs`), so every caller that
    /// reaches this is an `#[ignore]`d upstream cell.
    ///
    /// # Errors
    ///
    /// Always [`io::ErrorKind::Unsupported`].
    #[cfg(not(unix))]
    #[allow(dead_code)]
    pub fn start(_binary: DaemonBinary) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "inetd-mode test daemons need Unix socket stdio",
        ))
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

    /// Reads a first inetd session's greeting, blocking until the daemon sends
    /// it or its session ends.
    #[cfg(unix)]
    fn await_greeting(&self) -> io::Result<()> {
        use std::io::{BufRead, BufReader};
        let mut greeting = String::new();
        BufReader::new(TcpStream::connect(("127.0.0.1", self.port))?).read_line(&mut greeting)?;
        if greeting.starts_with("@RSYNCD: ") {
            return Ok(());
        }
        let stderr =
            fs::read_to_string(self.workdir.path().join("daemon.stderr")).unwrap_or_default();
        let log = self
            .log_contents()
            .unwrap_or_else(|_| String::from("(no log)"));
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "daemon session ended before its greeting (read {greeting:?})\nstderr: {stderr}\nlog: {log}"
            ),
        ))
    }
}

/// How a `TestDaemon` serves its listener.
#[cfg(unix)]
#[allow(
    dead_code,
    reason = "each variant is held for its Drop, which stops the daemon"
)]
enum Server {
    /// The oc-rsync daemon library, in a forked process.
    Forked(test_support::oc_daemon::OcDaemon),
    /// An upstream binary, started once per connection.
    Inetd(test_support::inetd_server::InetdServer),
}

/// The `rsyncd.conf` both upstream and oc-rsync serve the test module with.
#[cfg(unix)]
fn daemon_config(module_path: &Path) -> String {
    format!(
        "\
[testmodule]
    path = {path}
    comment = Test module for interop
    read only = false
    list = yes
    use chroot = false
    numeric ids = yes
",
        path = module_path.display()
    )
}

/// Create a test file with specific content, creating parent directories as needed.
#[allow(dead_code)]
pub fn create_test_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write test file");
}

/// Reports whether `path` is a genuine upstream rsync, judged by its
/// `--version` banner rather than by its location.
///
/// The path is not evidence: macOS ships `openrsync` at `/usr/bin/rsync`
/// (`openrsync: protocol version 29`), and an `oc-rsync` shim earlier on
/// `PATH` answers to the same name. Only upstream prints a first line of the
/// form `rsync  version N.N.N  protocol version NN`.
///
/// Returns the reason for rejection so callers can name it, rather than
/// reporting a bare "not found".
#[allow(dead_code)]
fn upstream_banner_check(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("absent".to_string());
    }
    let Ok(output) = Command::new(path).arg("--version").output() else {
        return Err("--version failed".to_string());
    };
    let banner = String::from_utf8_lossy(&output.stdout);
    let first = banner.lines().next().unwrap_or_default();
    if first.starts_with("rsync  version") && first.contains("protocol version") {
        return Ok(());
    }
    if first.is_empty() {
        // An empty banner is usually a binary that cannot execute here at all
        // rather than a different rsync - notably a Linux ELF left in the
        // shared `target/` by a container or a Linux host, which macOS refuses
        // with ENOEXEC. Report the status so that reads as "wrong platform"
        // instead of the misleading "not upstream: \"\"".
        return Err(format!("printed no version banner ({})", output.status));
    }
    Err(format!("not upstream: {first:?}"))
}

/// Panic unless `binary_path` holds a genuine upstream rsync.
///
/// Used by the version-pinned protocol-compatibility tests, which need one
/// *specific* upstream release rather than whichever one
/// [`upstream_rsync`] happens to resolve, so the path is fixed by the caller
/// and only its identity is verified here.
///
/// Existence alone is not enough: without the banner check a shim or a
/// non-upstream rsync sitting at the expected path would let a
/// cross-implementation test pass while comparing against the wrong oracle.
///
/// This fails loudly by design. A missing upstream binary must not be turned
/// into a silent early return - see [`upstream_rsync`] for why. The callers
/// are `#[ignore]`d, so a machine without the interop harness never reaches
/// this; running them with `--ignored` is an explicit request for the oracle.
#[allow(dead_code)]
pub fn require_upstream(binary_path: &str) {
    if let Err(reason) = upstream_banner_check(Path::new(binary_path)) {
        panic!(
            "upstream rsync required for this test, but {binary_path} is unusable ({reason}). \
             Build the pinned versions with `bash tools/ci/run_interop.sh`."
        );
    }
}

/// Candidate locations for a real upstream rsync, most specific first.
///
/// The interop harness installs versioned builds under `target/`; a developer
/// machine usually only has the packaged binary. Both are acceptable as long
/// as the binary is genuinely upstream, which [`upstream_rsync`] verifies from
/// `--version` rather than from the path it was found at.
/// The reference version is 3.5.1; the harness build and the source-tree build
/// come first so a machine with both uses the pinned one rather than whatever
/// the package manager shipped.
#[allow(dead_code)]
const UPSTREAM_CANDIDATES: &[&str] = &[
    UPSTREAM_3_5_1,
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/interop/upstream-src/rsync-3.5.1/rsync"
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
        match upstream_banner_check(path) {
            Ok(()) => return path.to_path_buf(),
            Err(reason) => rejected.push(format!("{candidate} ({reason})")),
        }
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
/// *writes* (`io.c:879`), so on a daemon pull the limit has to be applied by
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
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && is_temp_file_name(name)
            {
                temps.push(path);
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
/// (`io.c:879`), so on a daemon pull the limit belongs to the daemon and is
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
            dest: test_support::create_tempdir(),
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
