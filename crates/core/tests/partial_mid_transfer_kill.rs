//! Interop test: --partial mid-transfer kill retains partial file.
//!
//! Verifies that when a daemon transfer is interrupted mid-stream with
//! `--partial` enabled, the partially-received file is retained at the
//! destination rather than cleaned up. This mirrors upstream rsync behavior
//! from `cleanup.c:130-135` where `keep_partial && got_literal` retains
//! the temp file at the destination path.
//!
//! Test strategy:
//! 1. Start oc-rsync daemon serving a large (2 MB) test file
//! 2. Spawn oc-rsync client subprocess to pull with `--partial --bwlimit=8`
//! 3. Send SIGTERM to the client after partial data has been received
//! 4. Verify the partial file is retained at the destination
//! 5. Verify the retained file has some data (not empty, not complete)
//!
//! The `--bwlimit=8` (8 KB/s) slows the transfer to ~250 seconds for the
//! 2 MB file, giving a large kill window. The test interrupts the client
//! after 3 seconds, by which time ~24 KB should have been received.
//!
//! SIGTERM (not SIGKILL) is used because `--partial` relies on cooperative
//! shutdown: the signal handler sets the shutdown flag, the main thread
//! propagates shutdown through the SPSC channels, and the disk commit
//! thread retains the partial file before exiting.
//!
//! Upstream reference:
//! - `cleanup.c:105-135` - partial file retention on interrupt
//! - `receiver.c:340-345` - `do_rename(partialptr, fname)` on interrupt
//! - `options.c:keep_partial` - `--partial` flag

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{DaemonBinary, TestDaemon, create_test_file};

#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use tempfile::tempdir;

/// Size of the test file (2 MB) - large enough that a bandwidth-limited
/// transfer takes several seconds, giving a reliable kill window.
#[cfg(unix)]
const TEST_FILE_SIZE: usize = 2 * 1024 * 1024;

/// Bandwidth limit in KB/s for slowing the transfer.
/// At 8 KB/s, a 2 MB file takes ~256 seconds - far longer than our kill delay.
#[cfg(unix)]
const BWLIMIT_KBPS: u32 = 8;

/// How long to wait before interrupting the client (seconds).
/// At 8 KB/s, ~24 KB should be received in 3 seconds.
#[cfg(unix)]
const KILL_DELAY: Duration = Duration::from_secs(3);

/// Maximum time to wait for the client process to exit after signal.
#[cfg(unix)]
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// Locate the oc-rsync binary for subprocess spawning.
#[cfg(unix)]
fn locate_oc_rsync() -> Option<PathBuf> {
    // Try CARGO_BIN_EXE_oc-rsync first (set by cargo test)
    if let Some(path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    let current_exe = env::current_exe().ok()?;
    let mut dir = current_exe.parent()?;

    // Walk up to target/, checking each ancestor
    while !dir.ends_with("target") {
        let candidate = dir.join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }

    // Check common locations under target/
    for subdir in ["debug", "release"] {
        let candidate = dir.join(subdir).join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Generate deterministic test data (repeating pattern, not random).
///
/// Uses a repeating byte pattern so partial content can be verified as
/// a prefix of the full content, ruling out corruption.
#[cfg(unix)]
fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
}

/// Send SIGTERM to a child process and wait for it to exit.
///
/// SIGTERM triggers cooperative shutdown - the signal handler sets an
/// atomic flag, the main thread detects it and propagates shutdown
/// through the SPSC channels, allowing the disk commit thread to
/// retain partial files before exiting.
#[cfg(unix)]
fn sigterm_and_wait(child: &mut Child) {
    let pid = child.id();

    // Send SIGTERM for cooperative shutdown.
    // SAFETY: sending a signal to a known child process.
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(ret, 0, "failed to send SIGTERM to child pid {pid}");

    // Wait for the process to exit gracefully.
    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Last resort: SIGKILL if SIGTERM didn't work.
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("client pid {pid} did not exit within {EXIT_WAIT:?} after SIGTERM");
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("error waiting for client pid {pid}: {e}"),
        }
    }
}

/// Test that `--partial` retains the partially-received file when the
/// client is interrupted mid-transfer via SIGTERM.
///
/// This is the primary interop test for PIR-6.a: it exercises the full
/// daemon-to-client transfer path with a SIGTERM interrupt, verifying
/// that the disk commit thread's `retain_partial_file` logic works
/// end-to-end through the signal handler and cooperative shutdown path.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn partial_flag_retains_file_on_mid_transfer_kill() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    // Start oc-rsync daemon with a large test file.
    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("large.bin"), &test_data);

    let dest_dir = tempdir().expect("create dest dir");
    let dest_file = dest_dir.path().join("large.bin");

    // Spawn oc-rsync client with --partial and --bwlimit to slow transfer.
    let mut child = Command::new(&oc_rsync)
        .arg("--partial")
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/large.bin", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    // Wait for the transfer to make progress before interrupting.
    thread::sleep(KILL_DELAY);

    // Send SIGTERM for cooperative shutdown (partial retention requires it).
    sigterm_and_wait(&mut child);

    // Allow brief settling time for filesystem flush.
    thread::sleep(Duration::from_millis(500));

    // Verify: the partial file must exist at the destination.
    assert!(
        dest_file.exists(),
        "partial file must be retained at destination after SIGTERM; \
         daemon log: {}",
        daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    // Verify: the partial file must have some data (not empty).
    let partial_content = fs::read(&dest_file).expect("read partial file");
    assert!(
        !partial_content.is_empty(),
        "partial file must not be empty"
    );

    // Verify: the partial file must be smaller than the original
    // (the transfer was interrupted before completion).
    assert!(
        partial_content.len() < TEST_FILE_SIZE,
        "partial file ({} bytes) should be smaller than the full file ({} bytes); \
         if equal, the transfer completed before the interrupt",
        partial_content.len(),
        TEST_FILE_SIZE
    );

    // Verify: the partial content is a valid prefix of the original data,
    // ruling out corruption during the partial write.
    assert_eq!(
        &partial_content[..],
        &test_data[..partial_content.len()],
        "partial file content must be a valid prefix of the original data"
    );
}

/// Test that without `--partial`, temp files are cleaned up on interrupt.
///
/// This is the control test: verifies that the default behavior (no
/// `--partial`) removes temp files when the transfer is interrupted.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn no_partial_flag_cleans_up_on_mid_transfer_kill() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("large.bin"), &test_data);

    let dest_dir = tempdir().expect("create dest dir");
    let dest_file = dest_dir.path().join("large.bin");

    // Spawn client WITHOUT --partial.
    let mut child = Command::new(&oc_rsync)
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/large.bin", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    thread::sleep(KILL_DELAY);
    sigterm_and_wait(&mut child);

    thread::sleep(Duration::from_millis(500));

    // Without --partial, the destination file should NOT exist.
    // The temp file should have been cleaned up on shutdown.
    assert!(
        !dest_file.exists(),
        "without --partial, the destination file must not exist after interrupt; \
         found {} bytes at {}",
        dest_file.metadata().map(|m| m.len()).unwrap_or(0),
        dest_file.display()
    );

    // Also verify no temp files were left behind in the destination directory.
    let entries: Vec<_> = fs::read_dir(dest_dir.path())
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        entries.is_empty(),
        "no files should remain in destination directory without --partial; \
         found: {:?}",
        entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

/// Test that `--partial-dir` retains the partial file in the specified
/// directory on mid-transfer interrupt.
///
/// When `--partial-dir=DIR` is used and the transfer is interrupted, the
/// partial file should be placed in DIR relative to the destination,
/// not at the final destination path.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn partial_dir_flag_retains_file_in_directory_on_kill() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("large.bin"), &test_data);

    let dest_dir = tempdir().expect("create dest dir");
    let dest_file = dest_dir.path().join("large.bin");
    let partial_dir_name = ".rsync-partial";
    let partial_file = dest_dir.path().join(partial_dir_name).join("large.bin");

    // Spawn client with --partial-dir.
    let mut child = Command::new(&oc_rsync)
        .arg(format!("--partial-dir={partial_dir_name}"))
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/large.bin", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    thread::sleep(KILL_DELAY);
    sigterm_and_wait(&mut child);

    thread::sleep(Duration::from_millis(500));

    // The final destination should NOT have the file.
    assert!(
        !dest_file.exists(),
        "with --partial-dir, the file should NOT be at the final destination"
    );

    // The partial file should be in the partial directory.
    assert!(
        partial_file.exists(),
        "partial file must be retained in --partial-dir={partial_dir_name}; \
         daemon log: {}",
        daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    let partial_content = fs::read(&partial_file).expect("read partial file");
    assert!(
        !partial_content.is_empty(),
        "partial file in partial-dir must not be empty"
    );
    assert!(
        partial_content.len() < TEST_FILE_SIZE,
        "partial file ({} bytes) should be smaller than the full file ({} bytes)",
        partial_content.len(),
        TEST_FILE_SIZE
    );
}

/// Test that `--partial` works correctly with upstream rsync client
/// pulling from oc-rsync daemon.
///
/// This is the cross-implementation interop test: upstream rsync client
/// with `--partial` pulling from oc-rsync daemon, interrupted mid-transfer.
#[cfg(unix)]
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn upstream_client_partial_retains_file_on_kill() {
    let upstream_rsync = Path::new(common::UPSTREAM_3_4_1);
    if !upstream_rsync.exists() {
        eprintln!(
            "Skipping: upstream rsync 3.4.1 not found at {}",
            common::UPSTREAM_3_4_1
        );
        return;
    }

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("large.bin"), &test_data);

    let dest_dir = tempdir().expect("create dest dir");
    let dest_file = dest_dir.path().join("large.bin");

    // Spawn upstream rsync with --partial and --bwlimit.
    let mut child = Command::new(upstream_rsync)
        .arg("--partial")
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/large.bin", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn upstream rsync client");

    thread::sleep(KILL_DELAY);
    sigterm_and_wait(&mut child);

    thread::sleep(Duration::from_millis(500));

    // Upstream rsync with --partial should retain the partial file.
    assert!(
        dest_file.exists(),
        "upstream rsync --partial must retain partial file at destination; \
         daemon log: {}",
        daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    let partial_content = fs::read(&dest_file).expect("read partial file");
    assert!(
        !partial_content.is_empty(),
        "upstream rsync partial file must not be empty"
    );
    assert!(
        partial_content.len() < TEST_FILE_SIZE,
        "upstream rsync partial file ({} bytes) should be smaller than full ({} bytes)",
        partial_content.len(),
        TEST_FILE_SIZE
    );
}
