//! Regression: a client handed an empty file list must not enter its receive loop.
//!
//! upstream `main.c:1379-1391` `client_run()`:
//!
//! ```c
//! flist = recv_file_list(f_in, -1);
//! if (inc_recurse && file_total == 1)
//!         recv_additional_file_list(f_in);
//! if (flist && flist->used > 0) {
//!         ...
//!         exit_code2 = do_recv(f_in, f_out, local_name);
//! } else {
//!         handle_stats(-1);
//!         output_summary();
//! }
//! ```
//!
//! With no entries the client skips `do_recv()` entirely: it never writes an
//! ndx, never reads the sender's stats trailer (`handle_stats(-1)` short-circuits
//! on `f < 0 && !am_sender`, `main.c:362-363`), and never joins the goodbye
//! handshake. It prints the summary and exits on the `io_error` the sender packed
//! into the file-list end marker - 23 (`RERR_PARTIAL`) when a source path failed
//! to list, 0 when the list is legitimately empty.
//!
//! Our client used to keep reading, so the peer's FIN surfaced as
//! `transfer failed: failed to fill whole buffer` with no summary at all, and the
//! clean-empty-list case exited 23 where upstream exits 0. That asymmetry also
//! masked the server-side twin of this bug (`main.c:968-974`): an oc-to-oc test
//! could not tell a sender that wrongly entered `send_files()` from a healthy
//! one, because our client blocked either way.
//!
//! Both receiver drivers are exercised: `run_pipelined_incremental` when the
//! `transfer/incremental-flist` feature is on (CI's `--all-features` build) and
//! `run_pipelined` when it is off (the shipped binary's default). The gate lives
//! in each driver, so the assertions below must hold in both builds.
//!
//! Driving the real binary over a real socket is required: the empty-list path is
//! defined by what the two processes do and do not read off the wire.

use std::fs;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Upstream `RERR_PARTIAL`: some files were not transferred (`errcode.h`).
const RERR_PARTIAL: i32 = 23;

/// Wall-clock budget for one client run. Nothing is transferred in these
/// scenarios, so anything near this bound is the hang under test, not slowness.
const CLIENT_DEADLINE: Duration = Duration::from_secs(45);

/// How long to wait for a spawned daemon to bind its listening socket.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Path to the binary under test, resolved by Cargo at compile time so a stale
/// build in `target/` can never be picked up by a directory scan.
fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Locates an upstream rsync 3.x binary to run as a second sender, if installed.
///
/// Candidates, in order: an explicit `OC_RSYNC_UPSTREAM_RSYNC` override, the
/// binaries the interop harness installs under `target/interop`, then whatever
/// `rsync` is on `PATH`. Each must self-identify as `rsync version 3.x`, which
/// rejects the `openrsync` shim macOS ships as `/usr/bin/rsync`.
///
/// Returns `None` when nothing suitable exists; the caller skips that leg rather
/// than failing, since an upstream binary is an external resource.
fn upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    for version in ["3.4.4", "3.4.1", "3.1.3"] {
        candidates.push(PathBuf::from(format!(
            "target/interop/upstream-install/{version}/bin/rsync"
        )));
    }
    candidates.push(PathBuf::from("rsync"));

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .is_some_and(|text| {
                text.lines()
                    .next()
                    .is_some_and(|line| line.starts_with("rsync") && line.contains(" version 3."))
            })
    })
}

/// Reserves an ephemeral port by binding it and immediately releasing it.
fn allocate_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// One end of a child's stdio, boxed so both pipes share a drain loop.
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Pipe {
    fn into_reader(self) -> Box<dyn Read> {
        match self {
            Self::Out(out) => Box::new(out),
            Self::Err(err) => Box::new(err),
        }
    }
}

/// Takes both of a child's pipes, if present, tagged for reporting.
fn take_pipes(child: &mut Child) -> [(&'static str, Option<Pipe>); 2] {
    [
        ("stdout", child.stdout.take().map(Pipe::Out)),
        ("stderr", child.stderr.take().map(Pipe::Err)),
    ]
}

/// A spawned read-only rsync daemon serving one module, stdio drained
/// continuously so a full pipe buffer can never masquerade as the hang under
/// test.
struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    fn spawn(binary: &Path, config: &Path) -> Self {
        let port = allocate_port();
        let mut child = Command::new(binary)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--config")
            .arg(config)
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rsync daemon");

        for (_, pipe) in take_pipes(&mut child) {
            let Some(pipe) = pipe else { continue };
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = pipe.into_reader().read_to_end(&mut sink);
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

    fn url(&self, path: &str) -> String {
        format!("rsync://127.0.0.1:{}/mod/{path}", self.port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Writes a read-only single-module daemon config serving `module_root`.
fn write_config(dir: &Path, module_root: &Path) -> PathBuf {
    let config = dir.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\n[mod]\n    path = {}\n    read only = yes\n",
            module_root.display()
        ),
    )
    .expect("write daemon config");
    config
}

/// Outcome of one bounded client run.
struct ClientRun {
    code: i32,
    output: String,
}

impl ClientRun {
    /// The `sent N bytes  received N bytes  R bytes/sec` line upstream prints
    /// from `output_summary()` (`main.c:460-465`).
    fn has_summary(&self) -> bool {
        self.output.contains("sent ") && self.output.contains("bytes/sec")
    }
}

/// Runs one client pull to completion and returns its exit code and combined
/// output, failing the test if it does not finish inside [`CLIENT_DEADLINE`].
///
/// Both child pipes are drained on their own threads while we poll. Polling
/// `try_wait()` without draining deadlocks the moment the child fills a pipe
/// buffer, which would hang this harness for a reason unrelated to the bug under
/// test.
fn run_client_bounded(args: &[String], label: &str) -> ClientRun {
    let mut child = Command::new(oc_rsync_binary())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    let (tx, rx) = mpsc::channel::<String>();
    for (_, pipe) in take_pipes(&mut child) {
        let Some(pipe) = pipe else { continue };
        let tx = tx.clone();
        thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.into_reader().read_to_string(&mut text);
            let _ = tx.send(text);
        });
    }
    drop(tx);

    let deadline = Instant::now() + CLIENT_DEADLINE;
    let status = loop {
        match child.try_wait().expect("poll client") {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "{label}: client did not finish within {CLIENT_DEADLINE:?} \
                         (it re-entered the receive loop on an empty file list)"
                    );
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    };

    let output: String = rx.iter().collect();
    eprintln!("{label} client output:\n{output}");

    let code = status
        .code()
        .unwrap_or_else(|| panic!("{label}: client terminated by signal instead of exiting"));
    ClientRun { code, output }
}

/// Every sender the client is pulled against: always our own daemon, plus a real
/// upstream daemon when one is installed. Both legs matter - an oc-to-oc pair can
/// hide a divergence that both sides share.
fn senders() -> Vec<(String, PathBuf)> {
    let mut senders = vec![("oc-rsync".to_owned(), PathBuf::from(oc_rsync_binary()))];
    match upstream_rsync() {
        Some(upstream) => senders.push(("upstream-rsync".to_owned(), upstream)),
        None => eprintln!("skip upstream sender leg: no rsync 3.x binary available"),
    }
    senders
}

/// Asserts one client run ended the way upstream's empty-list arm does: bounded,
/// with the summary on stdout, no destination root created, and `expected_code`.
fn assert_empty_list_run(run: &ClientRun, dest: &Path, expected_code: i32, label: &str) {
    assert!(
        !run.output.contains("failed to fill whole buffer"),
        "{label}: client kept reading after an empty file list instead of \
         skipping do_recv() (upstream main.c:1383-1392)"
    );
    assert!(
        run.has_summary(),
        "{label}: upstream's empty-list arm still runs output_summary() \
         (main.c:1391); got:\n{}",
        run.output
    );
    assert!(
        !dest.exists(),
        "{label}: upstream never reaches get_local_name() on an empty list, so \
         the pre-flight mkdir at main.c:778-792 must not run"
    );
    assert_eq!(
        run.code, expected_code,
        "{label}: wrong exit code for an empty file list; got:\n{}",
        run.output
    );
}

/// Builds a module holding one readable file plus one unreadable directory, and
/// returns the temp root together with the module root.
fn module_with_locked_dir(locked: bool) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    let inner = module_root.join("locked");
    fs::create_dir_all(&inner).expect("create module root");
    fs::write(module_root.join("good.txt"), b"ok\n").expect("write good.txt");
    fs::write(inner.join("inner.txt"), b"inner\n").expect("write inner.txt");
    if locked {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&inner, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        }
    }
    (root, module_root)
}

/// A source path the sender cannot stat for a reason other than `ENOENT` yields
/// an empty file list whose end marker carries `IOERR_GENERAL`
/// (`flist.c:2427-2434` -> `flist.c:2508-2517`). Upstream prints the summary and
/// exits 23 (`cleanup.c:217-218`) without ever creating the destination root.
///
/// Unix-only: the fault is POSIX DAC (`chmod 000`), which root bypasses.
#[cfg(unix)]
#[test]
fn unreadable_source_path_prints_summary_and_exits_partial() {
    use std::os::unix::fs::PermissionsExt;

    // `id -u` keeps this test-only probe free of a libc dependency.
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|s| s.trim() == "0");
    if is_root {
        eprintln!("skip: root bypasses the chmod 000 traversal denial");
        return;
    }

    for (name, sender) in senders() {
        let (root, module_root) = module_with_locked_dir(true);
        let config = write_config(root.path(), &module_root);
        let daemon = Daemon::spawn(&sender, &config);

        // Trailing separator so the destination would be pre-created if the
        // empty-list gate were missing: `ensure_dest_root_exists` only fires for
        // a multi-file or trailing-slash operand.
        let dest = root.path().join("dest");
        let dest_arg = format!("{}{}", dest.display(), std::path::MAIN_SEPARATOR);
        let label = format!("unreadable-path via {name}");
        let run = run_client_bounded(
            &["-av".to_owned(), daemon.url("locked/inner.txt"), dest_arg],
            &label,
        );

        // Restore before the assertions so a failure still leaves a removable
        // temp tree behind.
        let _ = fs::set_permissions(
            module_root.join("locked"),
            fs::Permissions::from_mode(0o755),
        );
        assert_empty_list_run(&run, &dest, RERR_PARTIAL, &label);
    }
}

/// A source that every filter rule excludes yields an empty file list with *no*
/// io_error, so upstream prints the summary and exits 0. This is the leg that
/// pins the exit code down: our client used to report a transfer failure here
/// purely because its blocked read failed, turning a clean no-op into an error.
///
/// Runs on every platform - the trigger is a filter rule, not a permission bit -
/// so it is the Windows guard for this path as well.
#[test]
fn fully_excluded_source_prints_summary_and_exits_zero() {
    for (name, sender) in senders() {
        let (root, module_root) = module_with_locked_dir(false);
        let config = write_config(root.path(), &module_root);
        let daemon = Daemon::spawn(&sender, &config);

        let dest = root.path().join("dest");
        let dest_arg = format!("{}{}", dest.display(), std::path::MAIN_SEPARATOR);
        let label = format!("fully-excluded via {name}");
        let run = run_client_bounded(
            &[
                "-av".to_owned(),
                "--exclude=good.txt".to_owned(),
                daemon.url("good.txt"),
                dest_arg,
            ],
            &label,
        );

        assert_empty_list_run(&run, &dest, 0, &label);
    }
}
