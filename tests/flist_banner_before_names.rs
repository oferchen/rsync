//! The `sending incremental file list` banner must precede every per-file
//! name row, on every transport.
//!
//! Upstream prints the banner at the START of `send_file_list()` - before the
//! source walk emits anything - so it is always the first stdout line of a
//! recursive verbose push (flist.c:2248-2252, gated on `inc_recurse &&
//! INFO_GTE(FLIST, 1) && !am_server`). Verified against rsync 3.4.4
//! (protocol 32):
//!
//! ```text
//! $ rsync -nv -r src/ host:dest/
//! sending incremental file list
//! ./
//! f1
//! sub/
//! sub/f2
//! ...
//! ```
//!
//! oc-rsync used to render the sender banner only from the CLI's post-run
//! summary stage. A local copy renders its per-file rows from the same
//! deferred stage, so the order looked right there - but on an ssh or daemon
//! push the per-file rows stream to stdout live during the transfer, which
//! printed every name BEFORE the banner. The fix emits the banner at
//! file-list-send time from the client-side sender
//! (`generator/transfer/orchestrator.rs::announce_incremental_flist`), the
//! send-side twin of the receiver's early `receiving incremental file list`
//! banner.
//!
//! WHY this lives at the binary level: the mis-order was invisible to unit
//! tests because it is a cross-stage interleaving between the live transfer
//! output and the CLI's deferred renderer. Only a real two-process run
//! observes the actual stdout byte order.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);
const BANNER: &str = "sending incremental file list";

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read
/// with `env!`, not `env::var_os`: at run time it is unset and the lookup
/// would fall through to whatever stale `target/debug/oc-rsync` happens to be
/// on disk - silently testing a different build than the one just compiled.
fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Writes the `--rsh` shim.
///
/// oc-rsync invokes it with an SSH-style argv - `[<opts>..., <host>, <rsync
/// path>, "--server", ...]`. The shim drops the options and the host, then
/// execs the rest locally, giving a real two-process transfer over a pipe
/// pair without needing an SSH server.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    let body = "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         # $1 is the host placeholder; the server command follows it.\n\
         shift || true\n\
         exec \"$@\"\n";
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> Output {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");
    // Drain both pipes on their own threads. Polling try_wait() while the
    // pipes fill would deadlock: the child blocks writing into a full OS pipe
    // buffer and therefore never exits.
    let mut child_stdout = child.stdout.take().expect("child stdout");
    let mut child_stderr = child.stderr.take().expect("child stderr");
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("wait for client") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout_reader.join().unwrap_or_default(),
                    stderr: stderr_reader.join().unwrap_or_default(),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("client did not finish within {timeout:?}");
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Lays out a small recursive source tree: a file at the root and one in a
/// subdirectory, so the verbose listing has multiple name rows to mis-order.
fn setup() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let src = temp.path().join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("f1"), b"one\n").unwrap();
    fs::write(src.join("sub").join("f2"), b"two two\n").unwrap();
    (temp, src)
}

/// Asserts the upstream stdout contract for a recursive verbose push: the
/// banner is the FIRST line, printed exactly once, and the per-file rows
/// follow it.
fn assert_banner_first(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label}: transfer failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(BANNER),
        "{label}: banner must be the first stdout line (upstream flist.c:2248-2252 \
         prints it before the walk emits any per-file row)\nstdout:\n{stdout}",
    );
    assert_eq!(
        lines.iter().filter(|line| **line == BANNER).count(),
        1,
        "{label}: banner must print exactly once\nstdout:\n{stdout}",
    );
    for name in ["f1", "sub/f2"] {
        assert!(
            lines.contains(&name),
            "{label}: expected per-file row `{name}` after the banner\nstdout:\n{stdout}",
        );
    }
}

fn run_local(src: &Path, dest: &Path, dry_run: bool) -> Output {
    let mut cmd = Command::new(oc_rsync_binary());
    if dry_run {
        cmd.arg("-n");
    }
    cmd.arg("-v")
        .arg("-r")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dest.display()));
    spawn_with_timeout(cmd, RUN_TIMEOUT)
}

fn run_ssh_push(shim: &Path, src: &Path, dest: &Path, dry_run: bool) -> Output {
    let binary = oc_rsync_binary();
    let mut cmd = Command::new(&binary);
    if dry_run {
        cmd.arg("-n");
    }
    cmd.arg("-v")
        .arg("-r")
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/", src.display()))
        .arg(format!("fakehost:{}/", dest.display()));
    spawn_with_timeout(cmd, RUN_TIMEOUT)
}

/// Reserves an ephemeral port by binding it and immediately releasing it.
fn allocate_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// A spawned `oc-rsync --daemon` whose stdio is drained continuously; an
/// undrained pipe wedges the daemon once the OS buffer fills.
struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    fn spawn(root: &Path, module_root: &Path) -> Self {
        let config = root.join("rsyncd.conf");
        fs::write(
            &config,
            format!(
                "use chroot = no\n[mod]\n    path = {}\n    read only = no\n",
                module_root.display()
            ),
        )
        .expect("write daemon config");
        let port = allocate_port();
        let mut child = Command::new(oc_rsync_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--config")
            .arg(&config)
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn oc-rsync daemon");
        if let Some(mut out) = child.stdout.take() {
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = out.read_to_end(&mut sink);
            });
        }
        if let Some(mut err) = child.stderr.take() {
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = err.read_to_end(&mut sink);
            });
        }
        let daemon = Self { child, port };
        daemon.wait_until_listening();
        daemon
    }

    /// Polls the listening socket until the daemon accepts connections.
    fn wait_until_listening(&self) {
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let deadline = Instant::now() + DAEMON_READY_TIMEOUT;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&target, Duration::from_millis(250)).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon did not start listening on port {}", self.port);
    }

    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/mod/", self.port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_daemon_push(daemon: &Daemon, src: &Path, dry_run: bool) -> Output {
    let mut cmd = Command::new(oc_rsync_binary());
    if dry_run {
        cmd.arg("-n");
    }
    cmd.arg("-v")
        .arg("-r")
        .arg(format!("{}/", src.display()))
        .arg(daemon.url());
    spawn_with_timeout(cmd, RUN_TIMEOUT)
}

#[test]
fn local_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let dest = temp.path().join("dest");
    assert_banner_first(&run_local(&src, &dest, false), "local -v");
}

#[test]
fn local_dry_run_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let dest = temp.path().join("dest");
    assert_banner_first(&run_local(&src, &dest, true), "local -nv");
}

#[test]
fn ssh_push_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let shim = write_rsh_shim(temp.path());
    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    assert_banner_first(&run_ssh_push(&shim, &src, &dest, false), "ssh push -v");
}

#[test]
fn ssh_push_dry_run_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let shim = write_rsh_shim(temp.path());
    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    assert_banner_first(&run_ssh_push(&shim, &src, &dest, true), "ssh push -nv");
}

#[test]
fn daemon_push_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let module_root = temp.path().join("mod");
    fs::create_dir_all(&module_root).unwrap();
    let daemon = Daemon::spawn(temp.path(), &module_root);
    assert_banner_first(&run_daemon_push(&daemon, &src, false), "daemon push -v");
}

#[test]
fn daemon_push_dry_run_verbose_banner_precedes_names() {
    let (temp, src) = setup();
    let module_root = temp.path().join("mod");
    fs::create_dir_all(&module_root).unwrap();
    let daemon = Daemon::spawn(temp.path(), &module_root);
    assert_banner_first(&run_daemon_push(&daemon, &src, true), "daemon push -nv");
}
