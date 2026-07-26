//! Interop test: SIGTERM mid daemon transfer honours the `--partial` contract.
//!
//! A signal that arrives while the client is blocked reading delta data off
//! the wire must stop the transfer promptly, exit with `RERR_SIGNAL` (20), and
//! leave exactly what upstream leaves:
//!
//! | flags | destination after SIGTERM |
//! |---|---|
//! | `--partial` | the partial bytes at the final path |
//! | none | nothing (see `no_partial_temp_cleanup.rs`) |
//! | `--partial-dir=DIR` | the partial bytes in `DIR`, nothing at the final path |
//!
//! The transfer is made structurally unable to complete by interposing
//! [`common::CappingProxy`], which forwards a bounded number of daemon bytes
//! and then stalls with the connection still open. That is what lets the kill
//! be gated on observed on-disk progress instead of a timing guess, and it is
//! why `--bwlimit` is not used: upstream throttles socket *writes*
//! (`io.c:861`), so on a daemon pull the limit belongs to the daemon and is
//! inert on the client side.
//!
//! Upstream reference:
//! - `rsync.c:684 sig_int()` - records the signal, `exit_cleanup(RERR_SIGNAL)`
//! - `cleanup.c:159-183` - `cleanup_got_literal && keep_partial` retention,
//!   `handle_partial_dir(PDIR_CREATE)` for `--partial-dir`, modtime 0 stamp
//!   for plain `--partial`
//! - `errcode.h` - `RERR_SIGNAL` is 20

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{
    CappingProxy, DaemonBinary, PipeDrain, TestDaemon, create_test_file, upstream_rsync,
    wait_for_progress,
};

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use tempfile::{TempDir, tempdir};

/// Size of the test file. Comfortably larger than [`FORWARD_CAP`] so the
/// transfer cannot finish before the proxy stalls.
#[cfg(unix)]
const TEST_FILE_SIZE: usize = 2 * 1024 * 1024;

/// Daemon bytes the proxy forwards before stalling forever.
///
/// Large enough that the receiver has certainly opened its temp file and
/// written literal data (which is what arms upstream's `cleanup_got_literal`
/// gate), small enough that the file is nowhere near complete.
#[cfg(unix)]
const FORWARD_CAP: u64 = 512 * 1024;

/// How long to wait for the receiver to put bytes on disk before interrupting.
#[cfg(unix)]
const PROGRESS_WAIT: Duration = Duration::from_secs(20);

/// How long the client may take to exit after SIGTERM.
///
/// Upstream exits in ~0.4 s, dominated by the deliberate `msleep(400)` in
/// `rsync.c:694`. Anything in seconds here means the signal was not observed.
#[cfg(unix)]
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// rsync's exit code for SIGINT/SIGTERM/SIGHUP (`errcode.h` `RERR_SIGNAL`).
#[cfg(unix)]
const RERR_SIGNAL: i32 = 20;

/// Deterministic payload: a repeating byte pattern, so a retained partial can
/// be checked to be a genuine prefix of the source rather than merely the
/// right length.
#[cfg(unix)]
fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
}

/// A daemon, a stalling proxy in front of it, and a destination directory.
#[cfg(unix)]
struct StalledTransfer {
    daemon: TestDaemon,
    proxy: CappingProxy,
    dest: TempDir,
    data: Vec<u8>,
}

#[cfg(unix)]
impl StalledTransfer {
    fn start() -> Self {
        let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");
        let data = generate_test_data(TEST_FILE_SIZE);
        create_test_file(&daemon.module_path().join("large.bin"), &data);
        let proxy =
            CappingProxy::start(daemon.port(), FORWARD_CAP).expect("start byte-capping proxy");
        Self {
            daemon,
            proxy,
            dest: tempdir().expect("create dest dir"),
            data,
        }
    }

    fn source_url(&self) -> String {
        format!(
            "rsync://127.0.0.1:{}/testmodule/large.bin",
            self.proxy.port()
        )
    }

    fn dest_path(&self) -> &Path {
        self.dest.path()
    }

    /// Runs `client` with `flags`, waits for on-disk progress, sends SIGTERM,
    /// and returns the client's exit status.
    fn interrupt(&self, client: &Path, flags: &[&str]) -> ExitStatus {
        let mut child = Command::new(client)
            .args(flags)
            .arg("--timeout=60")
            .arg(self.source_url())
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
            self.daemon
                .log_contents()
                .unwrap_or_else(|_| "(unavailable)".into())
        );

        let status = sigterm_and_wait(&mut child);
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

/// Sends SIGTERM and waits for the child, failing the test if it does not go.
#[cfg(unix)]
fn sigterm_and_wait(child: &mut Child) -> ExitStatus {
    let pid = child.id();
    // SAFETY: sending a signal to a child this process spawned and has not reaped.
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(ret, 0, "failed to send SIGTERM to child pid {pid}");

    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "client pid {pid} ignored SIGTERM for {EXIT_WAIT:?}: the shutdown flag \
                         is set but nothing on the network path acted on it"
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("error waiting for client pid {pid}: {e}"),
        }
    }
}

/// `--partial` must leave the received prefix at the final destination path.
///
/// upstream: `cleanup.c:167-182` renames the temp onto `cleanup_new_fname`
/// when `keep_partial` is set and literal data arrived.
#[cfg(unix)]
#[test]
fn partial_flag_retains_file_on_mid_transfer_kill() {
    let fixture = StalledTransfer::start();
    fixture.interrupt(&test_support::oc_rsync_bin(), &["--partial"]);

    let dest_file = fixture.dest_path().join("large.bin");
    assert!(
        dest_file.exists(),
        "--partial must leave the partial at the destination; daemon log: {}",
        fixture
            .daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    let partial = fs::read(&dest_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "the proxy caps the transfer at {FORWARD_CAP} bytes, so a complete \
         {TEST_FILE_SIZE}-byte file means the interrupt was not what stopped it"
    );
    assert_eq!(
        &partial[..],
        &fixture.data[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}

/// Without `--partial` nothing may survive - neither the destination file nor
/// the `.name.XXXXXX` temp the receiver was writing.
///
/// upstream: `cleanup.c:194-197` unlinks `cleanup_fname` when `keep_partial`
/// is unset.
#[cfg(unix)]
#[test]
fn no_partial_flag_cleans_up_on_mid_transfer_kill() {
    let fixture = StalledTransfer::start();
    fixture.interrupt(&test_support::oc_rsync_bin(), &[]);

    let leftovers: Vec<_> = fs::read_dir(fixture.dest_path())
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "without --partial the destination must be empty after an interrupt; found: {leftovers:?}"
    );
}

/// `--partial-dir=DIR` must put the prefix in `DIR` and leave the final path
/// untouched.
///
/// upstream: `cleanup.c:167` routes the temp through
/// `handle_partial_dir(PDIR_CREATE)`, and unlike plain `--partial` it does not
/// stamp the modtime.
#[cfg(unix)]
#[test]
fn partial_dir_flag_retains_file_in_directory_on_kill() {
    let partial_dir_name = ".rsync-partial";
    let fixture = StalledTransfer::start();
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &[&format!("--partial-dir={partial_dir_name}")],
    );

    let dest_file = fixture.dest_path().join("large.bin");
    let partial_file = fixture.dest_path().join(partial_dir_name).join("large.bin");

    assert!(
        !dest_file.exists(),
        "--partial-dir must not leave anything at the final destination"
    );
    assert!(
        partial_file.exists(),
        "--partial-dir={partial_dir_name} must hold the partial; daemon log: {}",
        fixture
            .daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    let partial = fs::read(&partial_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "partial ({} bytes) must be shorter than the {TEST_FILE_SIZE}-byte source",
        partial.len()
    );
    assert_eq!(
        &partial[..],
        &fixture.data[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}

/// The same assertions against an upstream client, pinning oc's daemon side
/// and giving the oc-client expectations a live reference.
///
/// Opt-in like every other upstream-requiring test here, because the standard
/// test cells have no upstream rsync (macOS ships openrsync at
/// `/usr/bin/rsync`). Once selected it never skips itself: [`upstream_rsync`]
/// panics naming every path it tried rather than returning green.
#[cfg(unix)]
#[test]
#[ignore = "requires an upstream rsync binary"]
fn upstream_client_partial_retains_file_on_kill() {
    let upstream = upstream_rsync();
    let fixture = StalledTransfer::start();
    fixture.interrupt(&upstream, &["--partial"]);

    let dest_file = fixture.dest_path().join("large.bin");
    assert!(
        dest_file.exists(),
        "upstream --partial must leave the partial at the destination; daemon log: {}",
        fixture
            .daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    let partial = fs::read(&dest_file).expect("read partial file");
    assert!(!partial.is_empty(), "retained partial must not be empty");
    assert!(
        partial.len() < TEST_FILE_SIZE,
        "partial ({} bytes) must be shorter than the {TEST_FILE_SIZE}-byte source",
        partial.len()
    );
    assert_eq!(
        &partial[..],
        &fixture.data[..partial.len()],
        "the retained partial must be a byte-exact prefix of the source"
    );
}
