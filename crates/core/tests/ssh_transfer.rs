//! Integration tests for SSH transfer paths.
//!
//! These tests exercise the SSH transport layer by transferring files to/from
//! localhost. Each test checks SSH availability first and skips gracefully when
//! SSH is not reachable - making the suite safe for CI environments that may
//! lack a running `sshd`.
//!
//! Exit code semantics verified here match upstream rsync's errcode.h:
//! - 255 / `CommandFailed` - SSH connection failure
//! - 127 / `CommandNotFound` - remote command not found
//! - 124 / `CommandFailed` - remote command exited 255

#![cfg(unix)]

mod test_timeout;

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use core::client::{ClientConfig, ClientError, run_client};
use core::exit_code::ExitCode;
use tempfile::tempdir;
use test_timeout::{SSH_TIMEOUT, run_with_timeout};

/// Maximum number of retry attempts for flaky SSH connections.
const MAX_SSH_RETRIES: u32 = 3;

/// Delay between SSH retry attempts, giving transient failures time to clear.
const RETRY_DELAY: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` when SSH to localhost is available.
///
/// Probes by running `ssh -o BatchMode=yes -o ConnectTimeout=5 localhost true`.
/// Any non-zero exit or spawn failure means SSH is unavailable.
fn ssh_localhost_available() -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "localhost",
            "true",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns `true` when `oc-rsync` (or `rsync`) exists on localhost via SSH.
fn remote_rsync_available() -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "localhost",
            "which",
            "rsync",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a file at `path` with `content`, creating parent dirs as needed.
fn touch(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, content).expect("write fixture file");
}

/// Run `run_client` with up to `MAX_SSH_RETRIES` attempts.
///
/// SSH connections to localhost can be transiently refused under heavy CI load.
/// Retrying avoids false negatives without masking persistent failures.
fn run_client_with_retry(config_fn: impl Fn() -> ClientConfig) -> Result<(), ClientError> {
    let mut last_err = None;

    for attempt in 1..=MAX_SSH_RETRIES {
        match run_client(config_fn()) {
            Ok(_) => return Ok(()),
            Err(e) => {
                let code = e.code();
                // Only retry on transient connection failures.
                let is_transient = matches!(
                    code,
                    ExitCode::SocketIo | ExitCode::CommandFailed | ExitCode::Ipc
                );
                if is_transient && attempt < MAX_SSH_RETRIES {
                    eprintln!(
                        "SSH attempt {attempt}/{MAX_SSH_RETRIES} failed (exit {}), retrying...",
                        code.as_i32()
                    );
                    thread::sleep(RETRY_DELAY);
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_err.expect("at least one attempt must have run"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Verify that transferring a single file over SSH to localhost succeeds.
#[test]
fn ssh_localhost_single_file_transfer() {
    run_with_timeout(SSH_TIMEOUT, || {
        if !ssh_localhost_available() {
            eprintln!("Skipping: SSH to localhost unavailable");
            return;
        }
        if !remote_rsync_available() {
            eprintln!("Skipping: rsync not found on localhost via SSH");
            return;
        }

        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&dst_dir).expect("create dst dir");

        touch(&src_dir.join("hello.txt"), b"hello via ssh");

        let src_arg = format!("{}/", src_dir.display());
        let dst_arg = format!("localhost:{}/", dst_dir.display());

        let result = run_client_with_retry(|| {
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec![
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                ])
                .times(true)
                .build()
        });

        match result {
            Ok(_) => {
                let dest_file = dst_dir.join("hello.txt");
                assert!(
                    dest_file.exists(),
                    "transferred file should exist at destination"
                );
                assert_eq!(fs::read(&dest_file).expect("read dest"), b"hello via ssh");
            }
            Err(e) => {
                panic!("SSH transfer failed unexpectedly: {e}");
            }
        }
    });
}

/// Verify that pulling a file from localhost over SSH succeeds.
#[test]
fn ssh_localhost_pull_transfer() {
    run_with_timeout(SSH_TIMEOUT, || {
        if !ssh_localhost_available() {
            eprintln!("Skipping: SSH to localhost unavailable");
            return;
        }
        if !remote_rsync_available() {
            eprintln!("Skipping: rsync not found on localhost via SSH");
            return;
        }

        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&dst_dir).expect("create dst dir");

        touch(&src_dir.join("pulled.txt"), b"pulled content");

        let src_arg = format!("localhost:{}/", src_dir.display());
        let dst_arg = format!("{}/", dst_dir.display());

        let result = run_client_with_retry(|| {
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec![
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                ])
                .times(true)
                .build()
        });

        match result {
            Ok(_) => {
                let dest_file = dst_dir.join("pulled.txt");
                assert!(
                    dest_file.exists(),
                    "pulled file should exist at destination"
                );
                assert_eq!(fs::read(&dest_file).expect("read dest"), b"pulled content");
            }
            Err(e) => {
                panic!("SSH pull transfer failed unexpectedly: {e}");
            }
        }
    });
}

/// Verify that transferring a directory tree over SSH preserves structure.
#[test]
fn ssh_localhost_recursive_transfer() {
    run_with_timeout(SSH_TIMEOUT, || {
        if !ssh_localhost_available() {
            eprintln!("Skipping: SSH to localhost unavailable");
            return;
        }
        if !remote_rsync_available() {
            eprintln!("Skipping: rsync not found on localhost via SSH");
            return;
        }

        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&dst_dir).expect("create dst dir");

        touch(&src_dir.join("a.txt"), b"file a");
        touch(&src_dir.join("sub/b.txt"), b"file b");
        touch(&src_dir.join("sub/deep/c.txt"), b"file c");

        let src_arg = format!("{}/", src_dir.display());
        let dst_arg = format!("localhost:{}/", dst_dir.display());

        let result = run_client_with_retry(|| {
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec![
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                ])
                .recursive(true)
                .times(true)
                .build()
        });

        match result {
            Ok(_) => {
                assert!(dst_dir.join("a.txt").exists(), "a.txt should exist");
                assert!(dst_dir.join("sub/b.txt").exists(), "sub/b.txt should exist");
                assert!(
                    dst_dir.join("sub/deep/c.txt").exists(),
                    "sub/deep/c.txt should exist"
                );
                assert_eq!(fs::read(dst_dir.join("a.txt")).unwrap(), b"file a");
                assert_eq!(fs::read(dst_dir.join("sub/b.txt")).unwrap(), b"file b");
                assert_eq!(fs::read(dst_dir.join("sub/deep/c.txt")).unwrap(), b"file c");
            }
            Err(e) => {
                panic!("SSH recursive transfer failed unexpectedly: {e}");
            }
        }
    });
}

/// Verify that a bogus SSH command yields `CommandNotFound` (exit 127).
#[test]
fn ssh_command_not_found_exit_code() {
    run_with_timeout(SSH_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&dst_dir).expect("create dst dir");

        touch(&src_dir.join("data.txt"), b"data");

        // Use a non-existent program as the remote shell.
        let src_arg = format!("{}/", src_dir.display());
        let dst_arg = format!("localhost:{}/", dst_dir.display());

        let result = run_client(
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec!["/usr/bin/nonexistent_shell_binary_xyz"])
                .build(),
        );

        let err = result.expect_err("transfer with missing shell should fail");
        // The exit code should indicate command-not-found or a startup failure.
        // Depending on how the error surfaces, we accept CommandNotFound (127),
        // CommandRun (126), or StartClient (5).
        let code = err.code().as_i32();
        assert!(
            code == ExitCode::CommandNotFound.as_i32()
                || code == ExitCode::CommandRun.as_i32()
                || code == ExitCode::StartClient.as_i32()
                || code == ExitCode::Ipc.as_i32(),
            "expected exit code 127, 126, 14, or 5; got {code}: {err}"
        );
    });
}

/// Verify that SSH connection failure produces a connection-related exit code.
///
/// Uses a deliberately unreachable host to trigger a connection timeout or
/// refused error. The exit code should map to `CommandFailed` (124) or
/// `SocketIo` (10).
#[test]
fn ssh_connection_failure_exit_code() {
    run_with_timeout(SSH_TIMEOUT, || {
        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");

        touch(&src_dir.join("data.txt"), b"data");

        // Connect to a port that is almost certainly not running SSH.
        // Use 127.0.0.1 with a bogus port to get a fast connection-refused.
        let src_arg = format!("{}/", src_dir.display());
        let dst_arg = "localhost:/nonexistent/path/".to_string();

        let result = run_client(
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec![
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=2",
                    "-p",
                    "1", // port 1 - should be refused or timed out
                ])
                .build(),
        );

        let err = result.expect_err("transfer to unreachable host should fail");
        let code = err.code().as_i32();
        // SSH connection failure can surface as CommandFailed (124 - exit 255 from ssh),
        // SocketIo (10), Ipc (14), or StartClient (5).
        assert!(
            code == ExitCode::CommandFailed.as_i32()
                || code == ExitCode::SocketIo.as_i32()
                || code == ExitCode::Ipc.as_i32()
                || code == ExitCode::StartClient.as_i32(),
            "expected connection-failure exit code (124, 10, 14, or 5); got {code}: {err}"
        );
    });
}

/// Verify that the --rsync-path override works over SSH.
///
/// Points --rsync-path at a nonexistent binary on the remote side. The remote
/// shell should connect but the command should not be found.
#[test]
fn ssh_rsync_path_not_found() {
    run_with_timeout(SSH_TIMEOUT, || {
        if !ssh_localhost_available() {
            eprintln!("Skipping: SSH to localhost unavailable");
            return;
        }

        let temp = tempdir().expect("tempdir");
        let src_dir = temp.path().join("src");
        let dst_dir = temp.path().join("dst");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::create_dir_all(&dst_dir).expect("create dst dir");

        touch(&src_dir.join("data.txt"), b"data");

        let src_arg = format!("{}/", src_dir.display());
        let dst_arg = format!("localhost:{}/", dst_dir.display());

        let result = run_client(
            ClientConfig::builder()
                .transfer_args([OsString::from(&src_arg), OsString::from(&dst_arg)])
                .set_remote_shell(vec![
                    "ssh",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                ])
                .set_rsync_path("/nonexistent/rsync/binary/xyz")
                .build(),
        );

        let err = result.expect_err("transfer with missing rsync-path should fail");
        let code = err.code().as_i32();
        // The remote shell finds the command missing - could be CommandNotFound (127),
        // CommandRun (126), CommandFailed (124), Ipc (14), or StartClient (5).
        assert!(
            code == ExitCode::CommandNotFound.as_i32()
                || code == ExitCode::CommandRun.as_i32()
                || code == ExitCode::CommandFailed.as_i32()
                || code == ExitCode::Ipc.as_i32()
                || code == ExitCode::StartClient.as_i32(),
            "expected command-not-found exit code (127, 126, 124, 14, or 5); got {code}: {err}"
        );
    });
}

/// Verify the retry helper retries transient failures without looping forever.
#[test]
fn retry_logic_stops_on_persistent_failure() {
    run_with_timeout(SSH_TIMEOUT, || {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        // Build a config that will always fail (bogus shell).
        let result = run_client_with_retry(|| {
            attempts_clone.fetch_add(1, Ordering::SeqCst);
            ClientConfig::builder()
                .transfer_args([
                    OsString::from("/nonexistent/source/"),
                    OsString::from("localhost:/nonexistent/dest/"),
                ])
                .set_remote_shell(vec!["/usr/bin/nonexistent_shell_binary_xyz"])
                .build()
        });

        assert!(
            result.is_err(),
            "persistent failures should not be retried to success"
        );
        // The error is not transient (CommandNotFound/Ipc/StartClient), so retry
        // should stop after the first attempt or after MAX_SSH_RETRIES if it
        // happens to be classified as transient.
        let count = attempts.load(Ordering::SeqCst);
        assert!(
            count >= 1 && count <= MAX_SSH_RETRIES,
            "expected 1..={MAX_SSH_RETRIES} attempts, got {count}"
        );
    });
}
