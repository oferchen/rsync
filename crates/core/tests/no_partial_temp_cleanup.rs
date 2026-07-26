//! Interop test: SIGTERM mid daemon transfer leaves no residue without `--partial`.
//!
//! Default rsync writes each file to `.name.XXXXXX` and renames it on
//! completion. An interrupt must remove that temp: `cleanup.c:194-197` unlinks
//! `cleanup_fname` whenever `keep_partial` is unset. The failure this guards
//! against is an orphaned temp file - the destination looks clean in a casual
//! `ls` while a hidden partial silently consumes the disk.
//!
//! The transfer is made structurally unable to complete by interposing
//! [`common::CappingProxy`], which forwards a bounded number of daemon bytes
//! and then stalls with the connection open, so the interrupt can be gated on
//! observed on-disk progress rather than on a sleep.
//!
//! Upstream reference:
//! - `cleanup.c:159-197` - retention gate and the unlink that follows it
//! - `receiver.c` - temp file naming pattern `.filename.XXXXXX`
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
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use tempfile::{TempDir, tempdir};

/// Per-file payload size. Comfortably larger than [`FORWARD_CAP`].
#[cfg(unix)]
const TEST_FILE_SIZE: usize = 2 * 1024 * 1024;

/// Daemon bytes the proxy forwards before stalling forever.
#[cfg(unix)]
const FORWARD_CAP: u64 = 512 * 1024;

/// How long to wait for the receiver to put bytes on disk before interrupting.
#[cfg(unix)]
const PROGRESS_WAIT: Duration = Duration::from_secs(20);

/// How long the client may take to exit after SIGTERM.
#[cfg(unix)]
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// rsync's exit code for SIGINT/SIGTERM/SIGHUP (`errcode.h` `RERR_SIGNAL`).
#[cfg(unix)]
const RERR_SIGNAL: i32 = 20;

/// Deterministic payload (repeating byte pattern).
#[cfg(unix)]
fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
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

/// Whether a filename matches rsync's temp pattern `.name.XXXXXX`.
#[cfg(unix)]
fn is_temp_file_name(name: &str) -> bool {
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
fn find_temp_files(dir: &Path) -> Vec<PathBuf> {
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

/// A daemon, a stalling proxy in front of it, and a destination directory.
#[cfg(unix)]
struct StalledTransfer {
    daemon: TestDaemon,
    proxy: CappingProxy,
    dest: TempDir,
}

#[cfg(unix)]
impl StalledTransfer {
    fn start() -> Self {
        let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");
        let proxy =
            CappingProxy::start(daemon.port(), FORWARD_CAP).expect("start byte-capping proxy");
        Self {
            daemon,
            proxy,
            dest: tempdir().expect("create dest dir"),
        }
    }

    fn add_file(&self, name: &str, size: usize) {
        create_test_file(
            &self.daemon.module_path().join(name),
            &generate_test_data(size),
        );
    }

    fn module_url(&self) -> String {
        format!("rsync://127.0.0.1:{}/testmodule", self.proxy.port())
    }

    fn dest_path(&self) -> &Path {
        self.dest.path()
    }

    /// Runs `client` with `args`, waits for on-disk progress, sends SIGTERM,
    /// and asserts the interrupted exit code.
    fn interrupt(&self, client: &Path, args: &[String]) {
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
            self.daemon
                .log_contents()
                .unwrap_or_else(|_| "(unavailable)".into())
        );

        let status = sigterm_and_wait(&mut child);
        let (_stdout, stderr) = drain.join();
        assert_eq!(
            status.code(),
            Some(RERR_SIGNAL),
            "interrupted transfer must exit {RERR_SIGNAL}; stderr: {stderr}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// Single file, no `--partial`: neither the destination nor the temp survives.
#[cfg(unix)]
#[test]
fn no_partial_single_file_no_residue_on_kill() {
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &[format!("{}/large.bin", fixture.module_url())],
    );

    assert!(
        !fixture.dest_path().join("large.bin").exists(),
        "without --partial the destination file must not exist after an interrupt"
    );
    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans left behind: {orphans:?}"
    );
}

/// Recursive pull of several files: whatever completed may stay, but it must
/// be complete, and no temp may survive anywhere in the tree.
#[cfg(unix)]
#[test]
fn no_partial_multi_file_no_residue_on_kill() {
    let files = [
        ("small.txt", 256_usize),
        ("medium.bin", 64 * 1024),
        ("large_a.bin", TEST_FILE_SIZE),
        ("large_b.bin", TEST_FILE_SIZE),
    ];
    let fixture = StalledTransfer::start();
    for (name, size) in &files {
        fixture.add_file(name, *size);
    }
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &["-r".to_string(), format!("{}/", fixture.module_url())],
    );

    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans found after multi-file kill: {orphans:?}"
    );

    for (name, size) in &files {
        let dest_file = fixture.dest_path().join(name);
        if dest_file.exists() {
            let actual = fs::metadata(&dest_file).expect("stat dest file").len() as usize;
            assert_eq!(
                actual, *size,
                "{name} survived the interrupt at {actual} of {size} bytes; without --partial \
                 an incomplete file must be removed, not left looking finished"
            );
        }
    }
}

/// Nested tree: directories are created eagerly and legitimately remain, but
/// no `.name.XXXXXX` may be left at any depth.
#[cfg(unix)]
#[test]
fn no_partial_preserves_dirs_but_removes_temps() {
    let fixture = StalledTransfer::start();
    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        fixture.add_file(name, TEST_FILE_SIZE);
    }
    fixture.interrupt(
        &test_support::oc_rsync_bin(),
        &["-r".to_string(), format!("{}/", fixture.module_url())],
    );

    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "temp file orphans found in nested tree: {orphans:?}"
    );

    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        let dest_file = fixture.dest_path().join(name);
        if dest_file.exists() {
            let actual = fs::metadata(&dest_file).expect("stat").len() as usize;
            assert_eq!(
                actual, TEST_FILE_SIZE,
                "{name} survived the interrupt at {actual} of {TEST_FILE_SIZE} bytes"
            );
        }
    }
}

/// The same expectation against an upstream client, pinning oc's daemon side.
///
/// Opt-in like every other upstream-requiring test here, because the standard
/// test cells have no upstream rsync (macOS ships openrsync at
/// `/usr/bin/rsync`). Once selected it never skips itself: [`upstream_rsync`]
/// panics naming every path it tried rather than returning green.
#[cfg(unix)]
#[test]
#[ignore = "requires an upstream rsync binary"]
fn upstream_client_no_partial_cleans_up_on_kill() {
    let upstream = upstream_rsync();
    let fixture = StalledTransfer::start();
    fixture.add_file("large.bin", TEST_FILE_SIZE);
    fixture.interrupt(&upstream, &[format!("{}/large.bin", fixture.module_url())]);

    assert!(
        !fixture.dest_path().join("large.bin").exists(),
        "upstream without --partial must not leave a destination file; daemon log: {}",
        fixture
            .daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );
    let orphans = find_temp_files(fixture.dest_path());
    assert!(
        orphans.is_empty(),
        "upstream left temp file orphans: {orphans:?}"
    );
}

#[cfg(unix)]
#[cfg(test)]
mod temp_file_pattern_tests {
    use super::is_temp_file_name;

    /// The scanner must recognise the `.name.XXXXXX` shape rsync actually
    /// produces, including names that already contain dots.
    #[test]
    fn detects_rsync_temp_pattern() {
        assert!(is_temp_file_name(".large.bin.a1b2c3"));
        assert!(is_temp_file_name(".photo.jpg.D4E5F6"));
        assert!(is_temp_file_name(".a.b.c.d.AbCdEf"));
    }

    /// Ordinary dotfiles and short/long suffixes must not be reported as
    /// orphans, or the cleanup assertions would fail on innocent files.
    #[test]
    fn rejects_non_temp_names() {
        assert!(!is_temp_file_name(".bashrc"));
        assert!(!is_temp_file_name(".rsync-filter"));
        assert!(!is_temp_file_name("large.bin"));
        assert!(!is_temp_file_name(".large.bin.a1b2c"));
        assert!(!is_temp_file_name(".large.bin.a1b2c3d"));
        assert!(!is_temp_file_name(".large.bin.a1b2c-"));
    }
}
