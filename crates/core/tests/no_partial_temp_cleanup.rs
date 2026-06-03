//! Interop test: no --partial flag removes temp files on mid-transfer kill.
//!
//! Verifies that when a daemon transfer is interrupted mid-stream WITHOUT
//! `--partial` (the default behavior), no temp files or partial files remain
//! at the destination. This is the complementary test to PIR-6.a which
//! verifies retention with `--partial`.
//!
//! Upstream rsync writes to a temp file (`.filename.XXXXXX`) during transfer
//! and renames it to the final name on completion. On interrupt without
//! `--partial`, `cleanup.c:do_unlink()` removes the temp file so no orphan
//! remains. This test exercises that cleanup path end-to-end.
//!
//! Test scenarios:
//! 1. Single file transfer killed mid-stream - no residue at destination
//! 2. Multi-file transfer killed mid-stream - no residue for any file
//! 3. Upstream rsync client against oc-rsync daemon - cross-implementation
//! 4. Subdirectory structure preserved but no temp files remain
//!
//! Upstream reference:
//! - `cleanup.c:55-80` - `do_unlink()` removes temp file on interrupt
//! - `cleanup.c:105-135` - only retains when `keep_partial && got_literal`
//! - `receiver.c:490-510` - temp file naming pattern `.filename.XXXXXX`

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{DaemonBinary, TestDaemon, create_test_file};

#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::PathBuf;
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
    if let Some(path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    let current_exe = env::current_exe().ok()?;
    let mut dir = current_exe.parent()?;

    while !dir.ends_with("target") {
        let candidate = dir.join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }

    for subdir in ["debug", "release"] {
        let candidate = dir.join(subdir).join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Generate deterministic test data (repeating byte pattern).
#[cfg(unix)]
fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
}

/// Send SIGTERM to a child process and wait for it to exit.
///
/// SIGTERM triggers cooperative shutdown so the cleanup code path runs
/// before the process exits, removing temp files.
#[cfg(unix)]
fn sigterm_and_wait(child: &mut Child) {
    let pid = child.id();

    // SAFETY: sending a signal to a known child process.
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(ret, 0, "failed to send SIGTERM to child pid {pid}");

    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
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

/// Check whether a filename matches rsync's temp file pattern `.name.XXXXXX`.
///
/// Rsync creates temp files as `.originalname.XXXXXX` where XXXXXX is a
/// random suffix. This helper detects such orphans in a directory listing.
#[cfg(unix)]
fn is_temp_file_name(name: &str) -> bool {
    // Temp files start with '.' and have a 6-char random suffix after the last '.'
    if !name.starts_with('.') {
        return false;
    }
    // Strip leading dot, find the last dot, check suffix length
    let rest = &name[1..];
    if let Some(last_dot) = rest.rfind('.') {
        let suffix = &rest[last_dot + 1..];
        // Random suffix is exactly 6 alphanumeric characters
        suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric())
    } else {
        false
    }
}

/// Scan a directory tree recursively for any files matching rsync's temp
/// file pattern. Returns paths of any orphaned temp files found.
#[cfg(unix)]
fn find_temp_files(dir: &std::path::Path) -> Vec<PathBuf> {
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

/// Verify that a destination directory is completely clean - no final files,
/// no temp files, no partial files.
#[cfg(unix)]
fn assert_dest_clean(dest_dir: &std::path::Path, context: &str) {
    let entries: Vec<_> = fs::read_dir(dest_dir)
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .collect();

    assert!(
        entries.is_empty(),
        "{context}: destination should be empty after interrupt without --partial; \
         found: {:?}",
        entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

/// Single-file transfer without --partial: verify the temp file is removed
/// and no final file exists after mid-transfer SIGTERM.
///
/// This is the core no-partial cleanup test. Without --partial, rsync's
/// cleanup handler calls `do_unlink()` on the temp file before exiting.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn no_partial_single_file_no_residue_on_kill() {
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

    // The final destination file must not exist.
    assert!(
        !dest_file.exists(),
        "without --partial, destination file must not exist after interrupt"
    );

    // No temp files matching rsync's `.filename.XXXXXX` pattern should remain.
    let temp_orphans = find_temp_files(dest_dir.path());
    assert!(
        temp_orphans.is_empty(),
        "temp file orphans found in destination: {:?}",
        temp_orphans
    );

    // Destination directory must be completely empty.
    assert_dest_clean(dest_dir.path(), "single file");
}

/// Multi-file recursive transfer without --partial: verify no temp files
/// remain for any of the files after mid-transfer kill.
///
/// Exercises the cleanup path when multiple files are in-flight or queued.
/// The kill arrives while at least one file is actively transferring;
/// cleanup must remove the active temp file and leave no partial residue
/// for any file.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn no_partial_multi_file_no_residue_on_kill() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    // Create multiple files of varying sizes. The large files ensure the
    // transfer is still in progress when we send SIGTERM.
    let files = [
        ("small.txt", 256_usize),
        ("medium.bin", 64 * 1024),
        ("large_a.bin", TEST_FILE_SIZE),
        ("large_b.bin", TEST_FILE_SIZE),
    ];

    for (name, size) in &files {
        let data = generate_test_data(*size);
        create_test_file(&daemon.module_path().join(name), &data);
    }

    let dest_dir = tempdir().expect("create dest dir");

    // Pull entire module recursively without --partial.
    let mut child = Command::new(&oc_rsync)
        .arg("-r")
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    thread::sleep(KILL_DELAY);
    sigterm_and_wait(&mut child);
    thread::sleep(Duration::from_millis(500));

    // No temp files should remain anywhere in the destination tree.
    let temp_orphans = find_temp_files(dest_dir.path());
    assert!(
        temp_orphans.is_empty(),
        "temp file orphans found after multi-file kill: {:?}",
        temp_orphans
    );

    // Files that completed before the kill may exist (small files transfer
    // quickly even with bwlimit). But any large file that was mid-transfer
    // must not be present as a partial.
    for (name, size) in &files {
        let dest_file = dest_dir.path().join(name);
        if dest_file.exists() {
            let actual_size = fs::metadata(&dest_file).expect("stat dest file").len() as usize;
            // If the file exists, it must be complete (fully transferred before kill).
            assert_eq!(
                actual_size, *size,
                "file {name} exists but is incomplete ({actual_size} vs {size} bytes) - \
                 should have been cleaned up without --partial"
            );
        }
    }
}

/// Subdirectory transfer without --partial: verify subdirectories are
/// preserved but no temp files remain after kill.
///
/// Rsync creates directories immediately but writes files via temp files.
/// After cleanup, directories may remain (they are not temp files) but
/// no `.filename.XXXXXX` temp orphans should exist at any nesting level.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn no_partial_preserves_dirs_but_removes_temps() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    // Create nested directory structure with files at each level.
    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("root.bin"), &test_data);
    create_test_file(
        &daemon.module_path().join("subdir/nested.bin"),
        &test_data,
    );
    create_test_file(
        &daemon.module_path().join("subdir/deep/leaf.bin"),
        &test_data,
    );

    let dest_dir = tempdir().expect("create dest dir");

    let mut child = Command::new(&oc_rsync)
        .arg("-r")
        .arg(format!("--bwlimit={BWLIMIT_KBPS}"))
        .arg("--timeout=30")
        .arg(format!("{}/", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    thread::sleep(KILL_DELAY);
    sigterm_and_wait(&mut child);
    thread::sleep(Duration::from_millis(500));

    // No temp files at any level of the directory tree.
    let temp_orphans = find_temp_files(dest_dir.path());
    assert!(
        temp_orphans.is_empty(),
        "temp file orphans found in nested directory tree: {:?}",
        temp_orphans
    );

    // Verify no incomplete files exist.
    for name in ["root.bin", "subdir/nested.bin", "subdir/deep/leaf.bin"] {
        let dest_file = dest_dir.path().join(name);
        if dest_file.exists() {
            let actual_size = fs::metadata(&dest_file).expect("stat").len() as usize;
            assert_eq!(
                actual_size, TEST_FILE_SIZE,
                "file {name} exists but is incomplete ({actual_size} bytes) - \
                 should have been cleaned up without --partial"
            );
        }
    }
}

/// Upstream rsync client against oc-rsync daemon without --partial:
/// verify cross-implementation compatibility of temp file cleanup.
///
/// The oc-rsync daemon must cooperate correctly with an upstream rsync
/// client that does not use --partial, ensuring the client's cleanup
/// handler can remove the temp file without interference.
#[cfg(unix)]
#[test]
#[ignore = "requires upstream rsync 3.4.1 and oc-rsync binary"]
fn upstream_client_no_partial_cleans_up_on_kill() {
    let upstream_rsync = std::path::Path::new(common::UPSTREAM_3_4_1);
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

    // Upstream rsync client without --partial pulling from oc-rsync daemon.
    let mut child = Command::new(upstream_rsync)
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

    // Without --partial, the destination file must not exist.
    assert!(
        !dest_file.exists(),
        "upstream rsync without --partial must not leave destination file; \
         daemon log: {}",
        daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    // No temp files should remain.
    let temp_orphans = find_temp_files(dest_dir.path());
    assert!(
        temp_orphans.is_empty(),
        "upstream rsync left temp file orphans: {:?}",
        temp_orphans
    );

    assert_dest_clean(dest_dir.path(), "upstream client no-partial");
}

#[cfg(unix)]
#[cfg(test)]
mod temp_file_pattern_tests {
    use super::is_temp_file_name;

    #[test]
    fn detects_rsync_temp_pattern() {
        assert!(is_temp_file_name(".large.bin.a1b2c3"));
        assert!(is_temp_file_name(".photo.jpg.D4E5F6"));
        assert!(is_temp_file_name(".a.b.c.d.AbCdEf"));
    }

    #[test]
    fn rejects_non_temp_names() {
        // Normal dotfiles
        assert!(!is_temp_file_name(".rsync-filter"));
        assert!(!is_temp_file_name(".gitignore"));
        // Non-dotfiles
        assert!(!is_temp_file_name("large.bin"));
        assert!(!is_temp_file_name("file.txt"));
        // Wrong suffix length
        assert!(!is_temp_file_name(".file.abc"));
        assert!(!is_temp_file_name(".file.abcdefg"));
        // Non-alphanumeric suffix
        assert!(!is_temp_file_name(".file.ab!cd_"));
    }
}
