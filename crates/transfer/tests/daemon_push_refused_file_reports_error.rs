//! A daemon module that excludes a pushed file must refuse it out loud.
//!
//! # What this pins
//!
//! Upstream's receiver-side generator checks every entry against
//! `daemon_filter_list` and, on a match, reports
//! `ERROR: daemon refused to receive file "<name>"` as `FERROR_XFER`
//! (`generator.c:1275-1287`) before dropping the entry. Three consequences
//! follow, and all three are asserted here:
//!
//! 1. The pushing client renders the line. `FERROR_XFER` from a server is
//!    framed as `MSG_ERROR_XFER` (`log.c:330-346`) instead of being written to
//!    the daemon's own stderr, so the message travels to whoever ran the push.
//! 2. The push exits 23. `log.c:310-311` sets `got_xfer_error` on the frame and
//!    `cleanup.c:217-218` lifts a zero exit to `RERR_PARTIAL`. A silent skip
//!    would exit 0 and tell the user nothing was wrong while the module kept
//!    none of the refused files.
//! 3. The daemon's own stderr stays empty. A daemon that printed the
//!    diagnostic locally would satisfy assertion 2 for its own process while
//!    leaving the client with nothing to show.
//!
//! The pull direction is unaffected upstream - a module `exclude` hides the
//! file from the file list instead of refusing it - so a pull is asserted to
//! stay at exit 0 with no diagnostic. That guards against a fix that
//! over-reaches into the read path.
//!
//! Both file-list modes are covered: incremental recursion (the protocol-32
//! default) and `--no-inc-recursive`.
//!
//! When an upstream rsync 3.4.4 is available the same push is driven by it, so
//! the assertions are anchored to a real peer rather than to oc-on-oc, which
//! would hide a symmetric divergence.
//!
//! # Upstream References
//!
//! - `generator.c:1273-1287` - `check_filter(&daemon_filter_list, ...)` and the
//!   `ERROR: daemon refused to receive %s "%s"` report
//! - `log.c:310-311` - `case FERROR_XFER: got_xfer_error = 1;`
//! - `log.c:330-346` - `am_server` frames the text instead of printing it
//! - `cleanup.c:217-218` - `got_xfer_error` -> `RERR_PARTIAL` (23)
//! - `clientserver.c:874-893` - `rsync_module()` builds `daemon_filter_list`
//!   from the module's `exclude` / `include` / `filter` directives

#![cfg(unix)]

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use tempfile::{TempDir, tempdir};

/// The exact upstream text, minus the trailing newline.
///
/// upstream: generator.c:1281-1283.
const REFUSAL: &str = r#"ERROR: daemon refused to receive file "top.secret""#;

/// Writes an `rsyncd.conf` with one writable module that excludes `*.secret`.
///
/// `use chroot = false` keeps the daemon runnable unprivileged. `exclude` is
/// the directive that feeds `daemon_filter_list` (clientserver.c:891).
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    fs::write(
        config_path,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             max connections = 4\n\
             \n\
             [refuse]\n\
             path = {root}\n\
             read only = false\n\
             list = true\n\
             exclude = *.secret\n",
            pid = pid_path.display(),
            log = log_path.display(),
            root = module_root.display(),
        ),
    )
}

/// A running daemon plus a background drain of its stdout and stderr.
///
/// The pipes are drained on their own threads so a daemon that logs more than
/// a pipe buffer can never block mid-transfer, and so the captured stderr can
/// be inspected after the child is killed.
struct Daemon {
    child: Child,
    port: u16,
    stderr_rx: Receiver<String>,
    stdout_rx: Receiver<String>,
}

impl Daemon {
    /// Kills the daemon and returns everything it wrote to its own stderr.
    fn shutdown_stderr(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let err = self.stderr_rx.recv().unwrap_or_default();
        let _ = self.stdout_rx.recv();
        err
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drains one child pipe to completion on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<String> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Starts `<bin> --daemon --no-detach` on a free port with piped stdio.
fn spawn_daemon(bin: &Path, config_path: &Path) -> io::Result<Daemon> {
    let (mut child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    let stdout_rx = drain::<ChildStdout>(child.stdout.take());
    let stderr_rx = drain::<ChildStderr>(child.stderr.take());
    Ok(Daemon {
        child,
        port,
        stderr_rx,
        stdout_rx,
    })
}

/// Runs one client invocation to completion, returning `(exit code, stderr)`.
///
/// `Command::output()` reads both pipes concurrently, so a chatty client
/// cannot deadlock against a full pipe buffer.
fn run_client(bin: &Path, args: &[&str]) -> io::Result<(i32, String)> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Locates an upstream rsync speaking protocol 32, or `None`.
///
/// The `--version` banner is checked because a `rsync` on `PATH` is not
/// necessarily upstream rsync - macOS ships openrsync, which speaks protocol
/// 29 and knows nothing about this contract.
fn upstream_rsync() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let candidates = [
        workspace.join("target/interop/upstream-install/3.4.4/bin/rsync"),
        workspace.join("target/interop/upstream-src/rsync-3.4.4/rsync"),
    ];
    candidates
        .into_iter()
        .chain(test_support::locate_command_on_path("rsync"))
        .find(|path| {
            path.is_file()
                && Command::new(path)
                    .arg("--version")
                    .output()
                    .is_ok_and(|out| {
                        String::from_utf8_lossy(&out.stdout).contains("protocol version 32")
                    })
        })
}

/// Tempdir plus the paths every scenario needs.
struct Scratch {
    _tmp: TempDir,
    config: PathBuf,
    module: PathBuf,
    source: PathBuf,
    dest: PathBuf,
}

/// Builds a module root, a source tree holding one allowed and one excluded
/// file, and the daemon config binding them.
fn scratch() -> Option<Scratch> {
    let tmp = tempdir().ok()?;
    let root = tmp.path().to_path_buf();
    let module = root.join("module");
    let source = root.join("source");
    let dest = root.join("dest");
    fs::create_dir_all(&module).ok()?;
    fs::create_dir_all(&source).ok()?;
    fs::write(source.join("allowed.txt"), b"allowed\n").ok()?;
    fs::write(source.join("top.secret"), b"secret\n").ok()?;
    let config = root.join("rsyncd.conf");
    write_daemon_config(
        &config,
        &root.join("rsyncd.pid"),
        &root.join("rsyncd.log"),
        &module,
    )
    .ok()?;
    Some(Scratch {
        _tmp: tmp,
        config,
        module,
        source,
        dest,
    })
}

/// Drives one push and asserts the full upstream contract.
///
/// `extra` carries the file-list mode under test.
fn assert_push_is_refused(client: &Path, extra: &[&str]) {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = scratch() else {
        eprintln!("skipping: tempdir setup failed");
        return;
    };
    let Ok(daemon) = spawn_daemon(&oc_bin, &scratch.config) else {
        eprintln!("skipping: could not start the oc-rsync daemon");
        return;
    };

    let src = format!("{}/", scratch.source.display());
    let url = format!("rsync://127.0.0.1:{}/refuse/", daemon.port);
    let mut args = vec!["-a"];
    args.extend_from_slice(extra);
    args.push(src.as_str());
    args.push(url.as_str());
    let (code, stderr) = run_client(client, &args).expect("client must run");

    assert!(
        stderr.contains(REFUSAL),
        "the client must render the daemon's refusal (upstream generator.c:1281-1283); \
         a silent skip leaves the user with no diagnostic at all.\nargs: {args:?}\nstderr:\n{stderr}",
    );
    assert_eq!(
        code, 23,
        "a refused file must exit 23 (RERR_PARTIAL): FERROR_XFER sets got_xfer_error \
         (log.c:310-311) and cleanup.c:217-218 lifts the zero exit.\nargs: {args:?}\nstderr:\n{stderr}",
    );
    assert!(
        scratch.module.join("allowed.txt").is_file(),
        "the non-excluded file must still land in the module",
    );
    assert!(
        !scratch.module.join("top.secret").exists(),
        "the excluded file must never be written into the module",
    );

    let daemon_stderr = daemon.shutdown_stderr();
    assert!(
        !daemon_stderr.contains("daemon refused"),
        "the refusal must be framed to the client, not written to the daemon's own \
         stderr (upstream log.c:330-346 returns after send_msg under am_server)\n\
         daemon stderr:\n{daemon_stderr}",
    );
}

/// Incremental recursion on - the protocol-32 default for a recursive push.
#[test]
fn oc_client_push_of_excluded_file_is_refused_with_inc_recurse() {
    let oc_bin = test_support::oc_rsync_bin();
    assert_push_is_refused(&oc_bin, &[]);
}

/// Incremental recursion off - the receiver builds the whole list up front and
/// must reach the same verdict.
#[test]
fn oc_client_push_of_excluded_file_is_refused_without_inc_recurse() {
    let oc_bin = test_support::oc_rsync_bin();
    assert_push_is_refused(&oc_bin, &["--no-inc-recursive"]);
}

/// The same push driven by a real upstream client, so the text and the exit
/// code are pinned against upstream's own reader rather than oc's.
#[test]
fn upstream_client_push_of_excluded_file_is_refused() {
    let Some(upstream) = upstream_rsync() else {
        eprintln!("skipping: no upstream rsync speaking protocol 32 found");
        return;
    };
    assert_push_is_refused(&upstream, &[]);
    assert_push_is_refused(&upstream, &["--no-inc-recursive"]);
}

/// The read direction is untouched: upstream's module `exclude` keeps the file
/// out of the file list rather than refusing it, so a pull is a clean exit 0
/// with no diagnostic.
#[test]
fn pull_from_the_same_module_still_exits_zero() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = scratch() else {
        eprintln!("skipping: tempdir setup failed");
        return;
    };
    fs::write(scratch.module.join("allowed.txt"), b"allowed\n").expect("seed module");
    fs::write(scratch.module.join("top.secret"), b"secret\n").expect("seed module");
    fs::create_dir_all(&scratch.dest).expect("dest");
    let Ok(daemon) = spawn_daemon(&oc_bin, &scratch.config) else {
        eprintln!("skipping: could not start the oc-rsync daemon");
        return;
    };

    let url = format!("rsync://127.0.0.1:{}/refuse/", daemon.port);
    let dest = format!("{}/", scratch.dest.display());
    let (code, stderr) =
        run_client(&oc_bin, &["-a", url.as_str(), dest.as_str()]).expect("client must run");

    assert_eq!(
        code, 0,
        "a pull from an excluding module is a normal transfer upstream; the file is \
         absent from the list, never refused.\nstderr:\n{stderr}",
    );
    assert!(
        !stderr.contains("daemon refused"),
        "a pull must not produce a refusal diagnostic\nstderr:\n{stderr}",
    );
    assert!(
        scratch.dest.join("allowed.txt").is_file(),
        "the non-excluded file must be pulled",
    );
    assert!(
        !scratch.dest.join("top.secret").exists(),
        "the excluded file must stay hidden from the puller",
    );
}
