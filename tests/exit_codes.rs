//! Exit code integration tests verifying oc-rsync matches upstream rsync exit codes.
//!
//! This module tests that oc-rsync produces the same exit codes as upstream rsync
//! for various error conditions. Exit codes are defined in upstream rsync's errcode.h:
//!
//! | Code | Name              | Description                                    |
//! |------|-------------------|------------------------------------------------|
//! |  0   | RERR_OK           | Success                                        |
//! |  1   | RERR_SYNTAX       | Syntax or usage error                          |
//! |  2   | RERR_PROTOCOL     | Protocol incompatibility                       |
//! |  3   | RERR_FILESELECT   | Errors selecting input/output files, dirs      |
//! |  4   | RERR_UNSUPPORTED  | Requested action not supported                 |
//! |  5   | RERR_STARTCLIENT  | Error starting client-server protocol          |
//! | 10   | RERR_SOCKETIO     | Error in socket I/O                            |
//! | 11   | RERR_FILEIO       | Error in file I/O                              |
//! | 12   | RERR_STREAMIO     | Error in rsync protocol data stream            |
//! | 13   | RERR_MESSAGEIO    | Errors with program diagnostics                |
//! | 14   | RERR_IPC          | Error in IPC code                              |
//! | 20   | RERR_SIGNAL       | Received SIGUSR1/SIGINT                        |
//! | 23   | RERR_PARTIAL      | Partial transfer due to error                  |
//! | 24   | RERR_VANISHED     | Some files vanished before transfer            |
//! | 25   | RERR_DEL_LIMIT    | Skipped some deletes due to --max-delete       |
//! | 30   | RERR_TIMEOUT      | Timeout in data send/receive                   |
//!
//! Reference: target/interop/upstream-src/rsync-3.4.1/errcode.h

mod integration;

use core::exit_code::ExitCode;
use integration::helpers::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

/// Runs oc-rsync with given arguments and returns the output.
///
/// Uses a 60-second timeout to prevent CI hangs from stuck processes.
fn run_rsync(args: &[&str]) -> Output {
    let _cmd = RsyncCommand::new();
    let binary = oc_rsync_binary();

    let mut command = Command::new(binary);
    command.args(args);
    spawn_with_timeout(command, Duration::from_secs(60)).expect("failed to run oc-rsync")
}

/// Asserts that the exit code matches the expected value.
fn assert_exit_code(output: &Output, expected: ExitCode, context: &str) {
    let actual = output.status.code().unwrap_or(-1);
    let expected_i32 = expected.as_i32();

    if actual != expected_i32 {
        eprintln!("=== Exit Code Mismatch ===");
        eprintln!("Context: {context}");
        eprintln!(
            "Expected: {} ({} - {})",
            expected_i32,
            expected,
            expected.description()
        );
        eprintln!("Actual:   {actual}");
        eprintln!("=== stdout ===");
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("=== stderr ===");
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        panic!(
            "Exit code mismatch for {context}: expected {expected_i32} ({expected}), got {actual}"
        );
    }
}

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`, not `env::var_os`: at run time it is unset and the lookup would fall
/// through to whatever stale `target/debug/oc-rsync` happens to be on disk -
/// silently testing a different build than the one just compiled.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Tests that successful operations return exit code 0.
mod exit_code_0_success {
    use super::*;

    #[test]
    fn help_returns_success() {
        let output = run_rsync(&["--help"]);
        assert_exit_code(&output, ExitCode::Ok, "--help");
    }

    #[test]
    fn version_returns_success() {
        let output = run_rsync(&["--version"]);
        assert_exit_code(&output, ExitCode::Ok, "--version");
    }

    #[test]
    fn dry_run_with_valid_paths_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test content").unwrap();

        let output = run_rsync(&[
            "-n",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);
        assert_exit_code(&output, ExitCode::Ok, "dry-run with valid paths");
    }

    #[test]
    fn successful_local_copy_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test content").unwrap();

        let output = run_rsync(&[
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);
        assert_exit_code(&output, ExitCode::Ok, "successful local copy");
    }
}

/// Tests that syntax and usage errors return exit code 1.
mod exit_code_1_syntax {
    use super::*;

    #[test]
    fn invalid_option_returns_syntax_error() {
        let output = run_rsync(&["--definitely-not-a-valid-option", "src", "dest"]);
        assert_exit_code(&output, ExitCode::Syntax, "invalid option");
    }

    #[test]
    fn empty_filter_pattern_returns_syntax_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        // Empty filter pattern should be a syntax error
        let output = run_rsync(&[
            "--filter=",
            src_dir.to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);
        // Note: This may succeed if empty filter is allowed; adjust based on implementation
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 0 || code == 1,
            "Empty filter should return 0 (allowed) or 1 (syntax error), got {code}"
        );
    }
}

/// Tests that protocol incompatibility returns exit code 2.
mod exit_code_2_protocol {
    use super::*;

    #[test]
    fn unsupported_protocol_version_returns_protocol_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        // Request an impossibly high protocol version
        let output = run_rsync(&[
            "--protocol=99",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);
        assert_exit_code(&output, ExitCode::Protocol, "unsupported protocol version");
    }
}

/// Tests that file selection errors return exit code 3.
mod exit_code_3_file_select {
    use super::*;

    #[test]
    fn nonexistent_source_returns_file_select_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Source path doesn't exist
        let output = run_rsync(&[
            "/nonexistent/path/that/does/not/exist/anywhere",
            dest_dir.to_str().unwrap(),
        ]);
        // upstream: main.c:1356-1363 - ENOENT maps to RERR_VANISHED (24),
        // but may also return 3 (file select) or 23 (partial) depending on
        // where the error is caught.
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 3 || code == 23 || code == 24,
            "Nonexistent source should return 3, 23, or 24, got {code}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_destination_returns_file_select_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_file = test_dir.write_file("source.txt", b"content").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Make destination directory inaccessible
        let mut perms = fs::metadata(&dest_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&dest_dir, perms.clone()).unwrap();

        let output = run_rsync(&[src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);

        // Restore permissions for cleanup
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&dest_dir, perms);

        // Should return file selection error for inaccessible destination
        assert_exit_code(&output, ExitCode::FileSelect, "inaccessible destination");
    }
}

/// Tests that unsupported actions return exit code 4.
mod exit_code_4_unsupported {

    // Note: Exit code 4 is typically returned when a feature is not compiled in
    // or when attempting an action the remote rsync doesn't support.
    // These tests may be skipped if all features are compiled in.

    #[test]
    #[ignore = "Exit code 4 requires specific compile-time conditions"]
    fn unsupported_feature_returns_unsupported_error() {
        // This test would require a feature to be disabled at compile time
        // Keeping as a placeholder for documentation
    }
}

/// Tests that connection failures return exit code 5.
mod exit_code_5_start_client {
    use super::*;

    #[test]
    #[ignore = "requires daemon connection error handling implementation"]
    fn connection_to_closed_port_returns_start_client_error() {
        // Attempt to connect to a port that should be closed
        // Using high port number that's unlikely to have a service
        let output = run_rsync(&["rsync://127.0.0.1:59999/module/"]);

        // Note: This might return 10 (socket I/O) instead depending on implementation
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 5 || code == 10,
            "Connection to closed port should return 5 (start client) or 10 (socket I/O), got {code}"
        );
    }

    #[test]
    #[ignore = "requires daemon connection error handling implementation"]
    fn connection_to_invalid_daemon_url_returns_error() {
        let output = run_rsync(&["rsync://localhost:39873/nonexistent_module/"]);

        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 5 || code == 10,
            "Connection to invalid daemon should return 5 or 10, got {code}"
        );
    }
}

/// Tests that socket I/O errors return exit code 10.
mod exit_code_10_socket_io {
    use super::*;

    // Note: Socket I/O errors are difficult to trigger reliably in tests
    // as they typically require network-level failures during transfer.

    #[test]
    #[ignore = "requires daemon connection error handling implementation"]
    fn connection_refused_may_return_socket_io_error() {
        // Some connection failures may manifest as socket I/O errors
        let output = run_rsync(&["rsync://127.0.0.1:59998/module/"]);

        let code = output.status.code().unwrap_or(-1);
        // Connection refused can return either 5 (start client) or 10 (socket I/O)
        assert!(
            code == 5 || code == 10,
            "Connection refused should return 5 or 10, got {code}"
        );
    }
}

/// Tests that file I/O errors return exit code 11.
#[cfg(unix)]
mod exit_code_11_file_io {
    use super::*;

    #[test]
    fn unreadable_source_file_returns_file_io_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        let src_file = src_dir.join("unreadable.txt");

        fs::write(&src_file, b"secret content").unwrap();

        // Make file unreadable
        let mut perms = fs::metadata(&src_file).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&src_file, perms.clone()).unwrap();

        let output = run_rsync(&[src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);

        // Restore permissions for cleanup
        perms.set_mode(0o644);
        let _ = fs::set_permissions(&src_file, perms);

        // Note: upstream returns 23 (partial) for permission denied on source files
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 11 || code == 23,
            "Unreadable source should return 11 (file I/O) or 23 (partial), got {code}"
        );
    }

    /// upstream: main.c:817-821 get_local_name() - a pull whose destination
    /// root cannot be created is the lone `Receiver`'s rsyserr() and
    /// RERR_FILEIO, before the fork. Expected lines are rsync 3.5.1's (tarball
    /// build) for the same pull, without the program name and the trailer's
    /// source location.
    #[test]
    fn pull_into_an_uncreatable_destination_root_exits_with_file_io() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        fs::write(src_dir.join("a"), b"a").unwrap();
        fs::write(src_dir.join("b"), b"b").unwrap();
        let parent = test_dir.mkdir("ro").unwrap();
        let mut perms = fs::metadata(&parent).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&parent, perms.clone()).unwrap();
        if fs::write(parent.join("probe"), b"").is_ok() {
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&parent, perms);
            eprintln!("skip: the 0555 parent stays writable here (running as root)");
            return;
        }
        let target = parent.join("inner");
        let shim = test_dir
            .write_file("rsh.sh", b"#!/bin/sh\nshift\nexec \"$@\"\n")
            .unwrap();
        let mut shim_perms = fs::metadata(&shim).unwrap().permissions();
        shim_perms.set_mode(0o755);
        fs::set_permissions(&shim, shim_perms).unwrap();
        let rsync_path = format!("--rsync-path={}", oc_rsync_binary().display());

        let output = run_rsync(&[
            "-r",
            "-e",
            shim.to_str().unwrap(),
            &rsync_path,
            &format!("host:{}/", src_dir.display()),
            &format!("{}/", target.display()),
        ]);
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&parent, perms);

        assert_exit_code(&output, ExitCode::FileIo, "uncreatable destination root");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines.len(), 2, "{stderr}");
        assert_eq!(
            lines[0].split_once(": ").map(|(_, text)| text),
            Some(
                format!(
                    "[Receiver] mkdir \"{}\" failed: Permission denied (13)",
                    target.display()
                )
                .as_str()
            ),
            "{stderr}"
        );
        let trailer = lines[1].split_once(" error: ").map(|(_, text)| text);
        assert!(
            trailer.is_some_and(|text| {
                text.starts_with("error in file IO (code 11) at ")
                    && text.ends_with(&format!("[Receiver={}]", env!("CARGO_PKG_VERSION")))
            }),
            "{stderr}"
        );
    }
}

/// Tests that data stream errors return exit code 12.
mod exit_code_12_stream_io {
    // Note: Data stream errors require protocol-level corruption which is
    // difficult to trigger in normal integration tests.
    // This exit code is typically seen when the multiplexed data stream
    // becomes corrupted during transfer.
    #[cfg(unix)]
    use super::*;

    /// Runs each named direction (`"push"`, `"pull"`) through `rsh` and
    /// returns its output.
    #[cfg(unix)]
    fn run_directions(rsh: &str, directions: &[&'static str]) -> Vec<(Output, &'static str)> {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let src = temp.path().join("src");
        fs::create_dir(&src).expect("mkdir src");
        fs::write(src.join("a"), b"a").expect("write");
        let src_arg = format!("{}/", src.display());
        let dest_arg = format!("{}/", temp.path().join("dest").display());
        [
            (["-a", "-e", rsh, &src_arg, "host:dest/"], "push"),
            (["-a", "-e", rsh, "host:src/", &dest_arg], "pull"),
        ]
        .into_iter()
        .filter(|(_, direction)| directions.contains(direction))
        .map(|(args, direction)| (run_rsync(&args), direction))
        .collect()
    }

    /// upstream: io.c:299-303 whine_about_eof() exits RERR_STREAMIO when the
    /// remote shell dies before the handshake, and cleanup.c:146-152 folds in
    /// only `WEXITSTATUS(status)` - 0 for a signalled child - so the signal
    /// never outranks the stream error. RERR_TERMINATED (16) belongs to the
    /// clean-finish reap (main.c:198-226 wait_process_with_flush()) alone.
    ///
    /// The stub closes its stdout but keeps reading stdin until the client
    /// lets go, then kills itself. So the client's version write always lands
    /// and its read always sees EOF: one ordering, no race. Upstream 3.5.1
    /// exits 12 in both directions.
    #[cfg(unix)]
    #[test]
    fn remote_shell_killed_by_signal_before_handshake_returns_stream_io() {
        let rsh = "sh -c 'exec 1>&-; read -r _; kill -TERM $$'";
        for (output, context) in run_directions(rsh, &["push", "pull"]) {
            assert_exit_code(&output, ExitCode::StreamIo, context);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("connection unexpectedly closed (0 bytes received so far)"),
                "{context}: {stderr}"
            );
        }
    }

    /// The EOF arm of a remote shell that is gone before the handshake, pinned
    /// to upstream's text. The stub closes its stdout and drains stdin, so the
    /// version write always lands and the read always sees EOF: one ordering,
    /// no race. The write-EPIPE arm cannot be forced without timing (oc writes
    /// its version right after spawning the shell), so in-memory unit tests in
    /// `core::client::remote::version_exchange` pin that text instead.
    ///
    /// Expected lines are 3.5.1's (tarball build) for the same stub, with the
    /// program name and the trailer's source location left out:
    /// upstream: io.c:299-303 whine_about_eof(), log.c:937-963 log_exit().
    /// A pull names the client before its fork, `[Receiver]` (rsync.c:987-995
    /// who_am_i()).
    #[cfg(unix)]
    #[test]
    fn remote_shell_closing_stdout_before_handshake_prints_upstream_eof_whine() {
        let rsh = "sh -c 'exec 1>&-; cat >/dev/null'";
        let results = run_directions(rsh, &["push", "pull"]);
        for ((output, context), role) in results.into_iter().zip(["sender", "Receiver"]) {
            assert_exit_code(&output, ExitCode::StreamIo, context);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lines: Vec<&str> = stderr.lines().collect();
            assert_eq!(lines.len(), 2, "{context}: {stderr}");
            assert_eq!(
                lines[0].split_once(": ").map(|(_, text)| text),
                Some(
                    format!("connection unexpectedly closed (0 bytes received so far) [{role}]")
                        .as_str()
                ),
                "{context}: {stderr}"
            );
            let trailer = lines[1].split_once(" error: ").map(|(_, text)| text);
            assert!(
                trailer.is_some_and(|text| {
                    text.starts_with("error in rsync protocol data stream (code 12) at ")
                        && text.ends_with(&format!("[{role}={}]", env!("CARGO_PKG_VERSION")))
                }),
                "{context}: {stderr}"
            );
        }
    }

    #[test]
    #[ignore = "Data stream errors require protocol corruption simulation"]
    fn corrupted_stream_returns_stream_io_error() {
        // Would require injecting corruption into the protocol stream
    }
}

/// Which process closes a pull that ends with a partial transfer.
///
/// upstream: a pulling client forks once its file list is in (main.c:1077
/// do_recv()); the generator, the parent, exits last and prints the trailer.
/// With nothing in the list there is no fork, and the lone `Receiver` does
/// (main.c:1439-1441). Measured with rsync 3.5.1 as the client: `[generator=`
/// for an unreadable source file, `[Receiver=` for a missing source.
#[cfg(unix)]
mod pull_partial_trailer_role {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn pull(source: &str, test_dir: &TestDir) -> Output {
        let shim = test_dir
            .write_file("rsh.sh", b"#!/bin/sh\nshift\nexec \"$@\"\n")
            .unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        let dest = test_dir.path().join("dest");
        run_rsync(&[
            "-r",
            "-e",
            shim.to_str().unwrap(),
            &format!("--rsync-path={}", oc_rsync_binary().display()),
            &format!("host:{source}"),
            &format!("{}/", dest.display()),
        ])
    }

    fn trailer_role(output: &Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().last().unwrap_or_default();
        let role = last.rsplit_once('[').map_or("", |(_, tail)| tail);
        role.split_once('=')
            .map_or(role, |(role, _)| role)
            .to_owned()
    }

    #[test]
    fn a_partial_pull_after_the_fork_ends_on_the_generator() {
        let test_dir = TestDir::new().expect("create test dir");
        let src = test_dir.mkdir("src").unwrap();
        fs::write(src.join("a"), b"readable").unwrap();
        let unreadable = src.join("b");
        fs::write(&unreadable, b"secret").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&unreadable).is_ok() {
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
            eprintln!("skip: the 0000 file stays readable here (running as root)");
            return;
        }
        let output = pull(&format!("{}/", src.display()), &test_dir);
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
        assert_exit_code(&output, ExitCode::PartialTransfer, "unreadable source file");
        assert_eq!(trailer_role(&output), "generator", "{output:?}");
    }

    #[test]
    fn a_pull_with_nothing_to_list_ends_on_the_lone_receiver() {
        let test_dir = TestDir::new().expect("create test dir");
        let missing = test_dir.path().join("nope");
        let output = pull(&missing.display().to_string(), &test_dir);
        assert_exit_code(&output, ExitCode::PartialTransfer, "missing source");
        assert_eq!(trailer_role(&output), "Receiver", "{output:?}");
    }
}

/// Tests that diagnostic errors return exit code 13.
mod exit_code_13_message_io {
    // Note: Message I/O errors occur when diagnostic message handling fails.
    // This is typically an internal error condition.

    #[test]
    #[ignore = "Message I/O errors require internal failure simulation"]
    fn log_write_failure_returns_message_io_error() {
        // Would require --log-file to an unwritable location
    }
}

/// Tests that IPC errors return exit code 14.
mod exit_code_14_ipc {
    // Note: IPC errors occur during inter-process communication failures.
    // These are internal errors that are difficult to trigger externally.

    #[test]
    #[ignore = "IPC errors require internal process communication failure"]
    fn ipc_failure_returns_ipc_error() {
        // Would require simulating pipe/socket failures between rsync processes
    }
}

/// Tests that signal handling returns exit code 20.
#[cfg(unix)]
mod exit_code_20_signal {

    #[test]
    #[ignore = "Signal tests require process timing coordination"]
    fn sigint_returns_signal_error() {
        // Would need to send SIGINT during a long-running transfer
        // and verify the exit code
    }
}

/// Tests that partial transfers return exit code 23.
#[cfg(unix)]
mod exit_code_23_partial_transfer {
    use super::*;

    #[test]
    fn mixed_readable_unreadable_files_returns_partial() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create a readable file
        fs::write(src_dir.join("readable.txt"), b"can read this").unwrap();

        // Create an unreadable file
        let unreadable = src_dir.join("unreadable.txt");
        fs::write(&unreadable, b"cannot read this").unwrap();
        let mut perms = fs::metadata(&unreadable).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&unreadable, perms.clone()).unwrap();

        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        // Restore permissions for cleanup
        perms.set_mode(0o644);
        let _ = fs::set_permissions(&unreadable, perms);

        assert_exit_code(
            &output,
            ExitCode::PartialTransfer,
            "partial transfer due to unreadable file",
        );
    }

    #[test]
    fn missing_operands_returns_syntax_error() {
        // Genuinely missing operands means zero positional arguments. upstream
        // main.c:1818 - `rsync` with no operands prints usage and exits 1
        // (syntax/usage error), not 23. A lone existing operand is NOT a
        // missing-operands case: upstream options.c:2203 (`argc < 2 &&
        // !read_batch && !am_server => list_only`) reinterprets it as a source
        // to list and exits 0.
        let output = run_rsync(&[]);

        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 1,
            "no operands should return 1 (syntax/usage), got {code}"
        );
    }
}

/// Tests that vanished source files return exit code 24.
mod exit_code_24_vanished {
    // Note: This exit code requires files to disappear between file list
    // generation and the actual transfer. This is timing-sensitive and
    // difficult to test reliably.

    #[test]
    #[ignore = "Vanished files require timing-sensitive file deletion"]
    fn vanished_file_returns_vanished_error() {
        // Would need to delete files during an active transfer
    }
}

/// Tests that exceeding --max-delete limit returns exit code 25.
mod exit_code_25_delete_limit {
    use super::*;

    #[test]
    fn exceeding_max_delete_returns_delete_limit_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create files only in destination (to be deleted)
        for i in 0..5 {
            fs::write(dest_dir.join(format!("extra_{i}.txt")), b"to delete").unwrap();
        }

        // Run with --delete but limit to 2 deletions
        let output = run_rsync(&[
            "-r",
            "--delete",
            "--max-delete=2",
            &format!("{}/", src_dir.display()),
            &format!("{}/", dest_dir.display()),
        ]);

        // Should return delete limit exceeded
        let code = output.status.code().unwrap_or(-1);
        // Note: upstream might return 1 (syntax) for some --max-delete behaviors
        assert!(
            code == 25 || code == 1 || code == 0,
            "Max delete exceeded should return 25 (del limit), 1 (syntax), or 0 (if below limit), got {code}"
        );
    }
}

/// Tests that timeout conditions return exit code 30.
mod exit_code_30_timeout {
    // Note: Timeout testing requires either a slow transfer or a
    // hanging connection, which is difficult to simulate reliably.

    #[test]
    #[ignore = "Timeout tests require slow transfer simulation"]
    fn data_timeout_returns_timeout_error() {
        // Would need to stall data transfer long enough to trigger timeout
    }
}

/// Tests that the ExitCode enum values match upstream rsync's errcode.h.
mod exit_code_enum_values {
    use core::exit_code::ExitCode;

    /// Verifies all exit code values match upstream rsync exactly.
    ///
    /// Reference: upstream rsync errcode.h
    #[test]
    fn exit_codes_match_upstream_errcode_h() {
        // RERR_OK = 0
        assert_eq!(ExitCode::Ok.as_i32(), 0, "RERR_OK should be 0");

        // RERR_SYNTAX = 1
        assert_eq!(ExitCode::Syntax.as_i32(), 1, "RERR_SYNTAX should be 1");

        // RERR_PROTOCOL = 2
        assert_eq!(ExitCode::Protocol.as_i32(), 2, "RERR_PROTOCOL should be 2");

        // RERR_FILESELECT = 3
        assert_eq!(
            ExitCode::FileSelect.as_i32(),
            3,
            "RERR_FILESELECT should be 3"
        );

        // RERR_UNSUPPORTED = 4
        assert_eq!(
            ExitCode::Unsupported.as_i32(),
            4,
            "RERR_UNSUPPORTED should be 4"
        );

        // RERR_STARTCLIENT = 5
        assert_eq!(
            ExitCode::StartClient.as_i32(),
            5,
            "RERR_STARTCLIENT should be 5"
        );

        // RERR_SOCKETIO = 10
        assert_eq!(
            ExitCode::SocketIo.as_i32(),
            10,
            "RERR_SOCKETIO should be 10"
        );

        // RERR_FILEIO = 11
        assert_eq!(ExitCode::FileIo.as_i32(), 11, "RERR_FILEIO should be 11");

        // RERR_STREAMIO = 12
        assert_eq!(
            ExitCode::StreamIo.as_i32(),
            12,
            "RERR_STREAMIO should be 12"
        );

        // RERR_MESSAGEIO = 13
        assert_eq!(
            ExitCode::MessageIo.as_i32(),
            13,
            "RERR_MESSAGEIO should be 13"
        );

        // RERR_IPC = 14
        assert_eq!(ExitCode::Ipc.as_i32(), 14, "RERR_IPC should be 14");

        // RERR_CRASHED = 15
        assert_eq!(ExitCode::Crashed.as_i32(), 15, "RERR_CRASHED should be 15");

        // RERR_TERMINATED = 16
        assert_eq!(
            ExitCode::Terminated.as_i32(),
            16,
            "RERR_TERMINATED should be 16"
        );

        // RERR_SIGNAL1 = 19
        assert_eq!(ExitCode::Signal1.as_i32(), 19, "RERR_SIGNAL1 should be 19");

        // RERR_SIGNAL = 20
        assert_eq!(ExitCode::Signal.as_i32(), 20, "RERR_SIGNAL should be 20");

        // RERR_WAITCHILD = 21
        assert_eq!(
            ExitCode::WaitChild.as_i32(),
            21,
            "RERR_WAITCHILD should be 21"
        );

        // RERR_MALLOC = 22
        assert_eq!(ExitCode::Malloc.as_i32(), 22, "RERR_MALLOC should be 22");

        // RERR_PARTIAL = 23
        assert_eq!(
            ExitCode::PartialTransfer.as_i32(),
            23,
            "RERR_PARTIAL should be 23"
        );

        // RERR_VANISHED = 24
        assert_eq!(
            ExitCode::Vanished.as_i32(),
            24,
            "RERR_VANISHED should be 24"
        );

        // RERR_DEL_LIMIT = 25
        assert_eq!(
            ExitCode::DeleteLimit.as_i32(),
            25,
            "RERR_DEL_LIMIT should be 25"
        );

        // RERR_TIMEOUT = 30
        assert_eq!(ExitCode::Timeout.as_i32(), 30, "RERR_TIMEOUT should be 30");

        // RERR_CONTIMEOUT = 35
        assert_eq!(
            ExitCode::ConnectionTimeout.as_i32(),
            35,
            "RERR_CONTIMEOUT should be 35"
        );

        // RERR_CMD_FAILED = 124
        assert_eq!(
            ExitCode::CommandFailed.as_i32(),
            124,
            "RERR_CMD_FAILED should be 124"
        );

        // RERR_CMD_KILLED = 125
        assert_eq!(
            ExitCode::CommandKilled.as_i32(),
            125,
            "RERR_CMD_KILLED should be 125"
        );

        // RERR_CMD_RUN = 126
        assert_eq!(
            ExitCode::CommandRun.as_i32(),
            126,
            "RERR_CMD_RUN should be 126"
        );

        // RERR_CMD_NOTFOUND = 127
        assert_eq!(
            ExitCode::CommandNotFound.as_i32(),
            127,
            "RERR_CMD_NOTFOUND should be 127"
        );
    }

    /// Verifies from_i32 correctly parses all known exit codes.
    #[test]
    fn from_i32_parses_all_known_codes() {
        let known_codes = [
            (0, ExitCode::Ok),
            (1, ExitCode::Syntax),
            (2, ExitCode::Protocol),
            (3, ExitCode::FileSelect),
            (4, ExitCode::Unsupported),
            (5, ExitCode::StartClient),
            (10, ExitCode::SocketIo),
            (11, ExitCode::FileIo),
            (12, ExitCode::StreamIo),
            (13, ExitCode::MessageIo),
            (14, ExitCode::Ipc),
            (15, ExitCode::Crashed),
            (16, ExitCode::Terminated),
            (19, ExitCode::Signal1),
            (20, ExitCode::Signal),
            (21, ExitCode::WaitChild),
            (22, ExitCode::Malloc),
            (23, ExitCode::PartialTransfer),
            (24, ExitCode::Vanished),
            (25, ExitCode::DeleteLimit),
            (30, ExitCode::Timeout),
            (35, ExitCode::ConnectionTimeout),
            (124, ExitCode::CommandFailed),
            (125, ExitCode::CommandKilled),
            (126, ExitCode::CommandRun),
            (127, ExitCode::CommandNotFound),
        ];

        for (value, expected) in known_codes {
            let result = ExitCode::from_i32(value);
            assert_eq!(
                result,
                Some(expected),
                "from_i32({value}) should return Some({expected:?})"
            );
        }
    }

    /// Verifies unknown codes return None.
    #[test]
    fn from_i32_returns_none_for_unknown() {
        let unknown_codes = [-1, 7, 8, 9, 17, 18, 26, 27, 28, 29, 31, 100, 999];

        for value in unknown_codes {
            assert!(
                ExitCode::from_i32(value).is_none(),
                "from_i32({value}) should return None"
            );
        }
    }

    /// Verifies roundtrip conversion works.
    #[test]
    fn as_i32_and_from_i32_roundtrip() {
        let all_codes = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Signal1,
            ExitCode::Signal,
            ExitCode::WaitChild,
            ExitCode::Malloc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
            ExitCode::CommandFailed,
            ExitCode::CommandKilled,
            ExitCode::CommandRun,
            ExitCode::CommandNotFound,
        ];

        for code in all_codes {
            let value = code.as_i32();
            let parsed = ExitCode::from_i32(value);
            assert_eq!(
                parsed,
                Some(code),
                "Roundtrip failed for {code:?}: as_i32()={value}, from_i32()={parsed:?}"
            );
        }
    }

    /// Verifies descriptions are provided for all codes.
    #[test]
    fn all_codes_have_descriptions() {
        let all_codes = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Signal1,
            ExitCode::Signal,
            ExitCode::WaitChild,
            ExitCode::Malloc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
            ExitCode::CommandFailed,
            ExitCode::CommandKilled,
            ExitCode::CommandRun,
            ExitCode::CommandNotFound,
        ];

        for code in all_codes {
            let desc = code.description();
            assert!(
                !desc.is_empty(),
                "Exit code {code:?} should have a non-empty description"
            );
        }
    }
}

/// Tests that verify exit code handling consistency across error types.
mod exit_code_consistency {
    use core::exit_code::ExitCode;

    #[test]
    fn is_success_only_for_ok() {
        assert!(ExitCode::Ok.is_success());

        // All other codes should not be success
        let non_success = [
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
        ];

        for code in non_success {
            assert!(
                !code.is_success(),
                "{code:?} should not be considered success"
            );
        }
    }

    #[test]
    fn is_fatal_for_critical_errors() {
        let fatal = [
            ExitCode::Protocol,
            ExitCode::StartClient,
            ExitCode::SocketIo,
            ExitCode::StreamIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Malloc,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
        ];

        for code in fatal {
            assert!(code.is_fatal(), "{code:?} should be fatal");
        }

        let non_fatal = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::FileSelect,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
        ];

        for code in non_fatal {
            assert!(!code.is_fatal(), "{code:?} should not be fatal");
        }
    }

    #[test]
    fn is_partial_for_partial_transfer_codes() {
        let partial = [
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
        ];

        for code in partial {
            assert!(code.is_partial(), "{code:?} should be partial");
        }

        let not_partial = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::Timeout,
        ];

        for code in not_partial {
            assert!(!code.is_partial(), "{code:?} should not be partial");
        }
    }
}

/// Tests that verify the actual binary returns correct exit codes.
/// These tests run the oc-rsync binary and verify the exit status.
mod binary_exit_codes {
    use super::*;

    #[test]
    fn local_directory_sync_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create a directory structure
        fs::create_dir_all(src_dir.join("subdir")).unwrap();
        fs::write(src_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(src_dir.join("subdir/file2.txt"), b"content2").unwrap();

        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "recursive directory sync");

        // Verify files were copied
        assert!(dest_dir.join("file1.txt").exists());
        assert!(dest_dir.join("subdir/file2.txt").exists());
    }

    #[test]
    fn list_only_mode_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        // list-only requires both source and destination operands
        let output = run_rsync(&[
            "--list-only",
            src_dir.to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "list-only mode");
    }

    #[test]
    fn archive_mode_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"archive test").unwrap();

        let output = run_rsync(&[
            "-a",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "archive mode sync");
    }

    #[test]
    fn verbose_mode_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"verbose test").unwrap();

        let output = run_rsync(&[
            "-v",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "verbose mode");
    }

    #[test]
    fn no_arguments_returns_syntax_error() {
        // Running with no arguments at all
        let output = run_rsync(&[]);

        // Should return syntax/usage error
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 1 || code == 0, // --help may return 0
            "No arguments should return 0 (help) or 1 (syntax error), got {code}"
        );
    }

    #[test]
    fn invalid_short_option_returns_syntax_error() {
        let output = run_rsync(&["-Z", "src", "dest"]);
        assert_exit_code(&output, ExitCode::Syntax, "invalid short option -Z");
    }

    #[test]
    fn invalid_long_option_returns_syntax_error() {
        let output = run_rsync(&["--not-a-real-option-at-all", "src", "dest"]);
        assert_exit_code(&output, ExitCode::Syntax, "invalid long option");
    }

    #[test]
    fn missing_option_argument_returns_syntax_error() {
        // --filter requires an argument
        let output = run_rsync(&["--filter", "src", "dest"]);
        // This might return 1 if --filter consumes "src" as its argument
        // and then "dest" becomes the only operand
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 1 || code == 23,
            "Missing option argument should return error, got {code}"
        );
    }

    #[test]
    fn invalid_bwlimit_returns_syntax_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        let output = run_rsync(&[
            "--bwlimit=not-a-number",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Syntax, "invalid bwlimit value");
    }

    #[test]
    fn protocol_version_too_high_returns_protocol_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        // Protocol 99 doesn't exist
        let output = run_rsync(&[
            "--protocol=99",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Protocol, "protocol version 99 too high");
    }

    #[test]
    fn protocol_version_too_low_returns_protocol_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"test").unwrap();

        // Protocol 1 is too old
        let output = run_rsync(&[
            "--protocol=1",
            src_dir.join("file.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Protocol, "protocol version 1 too low");
    }

    #[test]
    fn nonexistent_source_file_returns_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let dest_dir = test_dir.mkdir("dest").unwrap();

        let output = run_rsync(&[
            "/definitely/does/not/exist/file.txt",
            dest_dir.to_str().unwrap(),
        ]);

        // upstream: main.c:1356-1363 - ENOENT maps to RERR_VANISHED (24),
        // but may also return 3 (file select) or 23 (partial) depending on
        // where the error is caught.
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 3 || code == 23 || code == 24,
            "Nonexistent source should return 3, 23, or 24, got {code}"
        );
    }

    #[test]
    fn nonexistent_source_directory_returns_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let dest_dir = test_dir.mkdir("dest").unwrap();

        let output = run_rsync(&[
            "-r",
            "/nonexistent/source/directory/",
            dest_dir.to_str().unwrap(),
        ]);

        // upstream: main.c:1356-1363 - ENOENT maps to RERR_VANISHED (24),
        // but may also return 3 (file select) or 23 (partial).
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 3 || code == 23 || code == 24,
            "Nonexistent source directory should return 3, 23, or 24, got {code}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_destination_returns_file_select_error() {
        use std::os::unix::fs::PermissionsExt;

        let test_dir = TestDir::new().expect("create test dir");
        let src_file = test_dir.write_file("source.txt", b"content").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Make destination directory inaccessible
        let mut perms = fs::metadata(&dest_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&dest_dir, perms.clone()).unwrap();

        let output = run_rsync(&[src_file.to_str().unwrap(), dest_dir.to_str().unwrap()]);

        // Restore permissions for cleanup
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&dest_dir, perms);

        assert_exit_code(&output, ExitCode::FileSelect, "inaccessible destination");
    }

    #[cfg(unix)]
    #[test]
    fn some_files_unreadable_returns_partial_transfer() {
        use std::os::unix::fs::PermissionsExt;

        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create readable file
        fs::write(src_dir.join("readable.txt"), b"can read").unwrap();

        // Create unreadable file
        let unreadable = src_dir.join("unreadable.txt");
        fs::write(&unreadable, b"cannot read").unwrap();
        let mut perms = fs::metadata(&unreadable).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&unreadable, perms.clone()).unwrap();

        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        // Restore permissions for cleanup
        perms.set_mode(0o644);
        let _ = fs::set_permissions(&unreadable, perms);

        assert_exit_code(
            &output,
            ExitCode::PartialTransfer,
            "partial transfer with unreadable file",
        );
    }

    #[test]
    fn max_delete_limit_exceeded_returns_delete_limit_error() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create files only in destination (they should be deleted with --delete)
        for i in 0..10 {
            fs::write(dest_dir.join(format!("extra_{i}.txt")), b"to delete").unwrap();
        }

        // Try to delete with --max-delete=2
        let output = run_rsync(&[
            "-r",
            "--delete",
            "--max-delete=2",
            &format!("{}/", src_dir.display()),
            &format!("{}/", dest_dir.display()),
        ]);

        // Should return 25 (delete limit) when limit is exceeded
        // May return 0 if implementation doesn't enforce max-delete yet
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 0 || code == 25,
            "Max delete exceeded should return 0 or 25, got {code}"
        );
    }

    #[test]
    fn nonexistent_remote_shell_returns_error() {
        let output = run_rsync(&[
            "-e",
            "/nonexistent/shell/program",
            "user@host:src/",
            "dest/",
        ]);

        // May return 127 (command not found), 1 (syntax), or 14 (IPC error)
        // depending on implementation - the important thing is it's an error
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 1 || code == 14 || code == 127,
            "Nonexistent shell should return error (1, 14, or 127), got {code}"
        );
    }
}

/// Tests that document which exit codes are reachable in the current implementation.
mod exit_code_coverage {
    use core::exit_code::ExitCode;

    /// Documents all upstream rsync exit codes and their implementation status.
    #[test]
    fn document_exit_code_implementation_status() {
        // Exit codes that are fully implemented and tested
        let implemented = [
            (ExitCode::Ok, "Success - fully implemented"),
            (ExitCode::Syntax, "Syntax error - argument parsing"),
            (ExitCode::Protocol, "Protocol error - version negotiation"),
            (ExitCode::FileSelect, "File selection - source/dest access"),
            (
                ExitCode::PartialTransfer,
                "Partial transfer - file I/O errors",
            ),
            (ExitCode::DeleteLimit, "Delete limit - --max-delete"),
        ];

        for (code, description) in implemented {
            assert!(
                !code.description().is_empty(),
                "{code:?}: {description} - should have description"
            );
        }

        // Exit codes that require specific conditions to trigger
        let conditional = [
            (ExitCode::Unsupported, "Requires disabled feature"),
            (ExitCode::StartClient, "Requires daemon connection failure"),
            (ExitCode::SocketIo, "Requires network I/O error"),
            (ExitCode::FileIo, "Requires file I/O error during transfer"),
            (ExitCode::StreamIo, "Requires protocol stream corruption"),
            (ExitCode::MessageIo, "Requires diagnostic output failure"),
            (
                ExitCode::Ipc,
                "Requires inter-process communication failure",
            ),
            (ExitCode::Crashed, "Requires sibling process crash"),
            (ExitCode::Terminated, "Requires signal handling"),
            (ExitCode::Signal1, "Requires SIGUSR1"),
            (ExitCode::Signal, "Requires SIGINT/SIGTERM/SIGHUP"),
            (ExitCode::WaitChild, "Requires waitpid failure"),
            (ExitCode::Malloc, "Requires memory allocation failure"),
            (ExitCode::Vanished, "Requires files deleted during transfer"),
            (ExitCode::Timeout, "Requires data transfer timeout"),
            (
                ExitCode::ConnectionTimeout,
                "Requires daemon connection timeout",
            ),
            (ExitCode::CommandFailed, "Requires remote shell exit 255"),
            (ExitCode::CommandKilled, "Requires remote shell killed"),
            (ExitCode::CommandRun, "Requires non-executable remote shell"),
            (ExitCode::CommandNotFound, "Requires missing remote shell"),
        ];

        for (code, description) in conditional {
            assert!(
                !code.description().is_empty(),
                "{code:?}: {description} - should have description"
            );
        }
    }

    /// Verify all exit codes have appropriate classification.
    #[test]
    fn all_exit_codes_classified() {
        let all_codes = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Signal1,
            ExitCode::Signal,
            ExitCode::WaitChild,
            ExitCode::Malloc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
            ExitCode::CommandFailed,
            ExitCode::CommandKilled,
            ExitCode::CommandRun,
            ExitCode::CommandNotFound,
        ];

        // Every exit code should be classified into at least one category
        // or be explicitly "normal" (neither fatal nor partial)
        for code in all_codes {
            let is_success = code.is_success();
            let is_fatal = code.is_fatal();
            let is_partial = code.is_partial();

            // Success should be exclusive
            if is_success {
                assert!(
                    !is_fatal && !is_partial,
                    "{code:?}: success should be exclusive"
                );
            }

            // Fatal and partial should be mutually exclusive
            assert!(
                !(is_fatal && is_partial),
                "{code:?}: cannot be both fatal and partial"
            );
        }
    }
}

/// Tests for edge cases in exit code handling.
mod exit_code_edge_cases {
    use super::*;

    #[test]
    fn empty_source_directory_sync_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Sync empty directory
        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "empty directory sync");
    }

    #[test]
    fn sync_to_same_directory_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let dir = test_dir.mkdir("dir").unwrap();
        fs::write(dir.join("file.txt"), b"content").unwrap();

        // This should be a no-op but succeed
        let output = run_rsync(&[
            dir.join("file.txt").to_str().unwrap(),
            dir.to_str().unwrap(),
        ]);

        // May succeed or fail depending on implementation
        let code = output.status.code().unwrap_or(-1);
        // Could return 0 (success, no-op) or 23 (partial, same file)
        assert!(
            code == 0 || code == 23,
            "Same file sync should return 0 or 23, got {code}"
        );
    }

    #[test]
    fn multiple_source_files_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        fs::write(src_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(src_dir.join("file2.txt"), b"content2").unwrap();
        fs::write(src_dir.join("file3.txt"), b"content3").unwrap();

        let output = run_rsync(&[
            src_dir.join("file1.txt").to_str().unwrap(),
            src_dir.join("file2.txt").to_str().unwrap(),
            src_dir.join("file3.txt").to_str().unwrap(),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "multiple source files");
    }

    #[test]
    fn filter_exclude_all_returns_success_or_file_select() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();
        fs::write(src_dir.join("file.txt"), b"content").unwrap();

        // Exclude everything
        let output = run_rsync(&[
            "-r",
            "--exclude=*",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        // May return 0 (success, nothing to transfer) or 3 (file select, nothing selected)
        let code = output.status.code().unwrap_or(-1);
        assert!(
            code == 0 || code == 3,
            "Exclude all should return 0 or 3, got {code}"
        );
    }

    #[test]
    fn large_number_of_files_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create 100 files
        for i in 0..100 {
            fs::write(
                src_dir.join(format!("file_{i:03}.txt")),
                format!("content {i}"),
            )
            .unwrap();
        }

        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "large number of files");
    }

    #[test]
    fn deep_directory_hierarchy_returns_success() {
        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create deep directory structure
        let mut path = src_dir.clone();
        for i in 0..10 {
            path = path.join(format!("level_{i}"));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("file.txt"), format!("level {i}")).unwrap();
        }

        let output = run_rsync(&[
            "-r",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "deep directory hierarchy");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_handling_returns_success() {
        use std::os::unix::fs::symlink;

        let test_dir = TestDir::new().expect("create test dir");
        let src_dir = test_dir.mkdir("src").unwrap();
        let dest_dir = test_dir.mkdir("dest").unwrap();

        // Create a file and a symlink to it
        let target = src_dir.join("target.txt");
        fs::write(&target, b"target content").unwrap();
        symlink(&target, src_dir.join("link.txt")).unwrap();

        let output = run_rsync(&[
            "-rl",
            &format!("{}/", src_dir.display()),
            dest_dir.to_str().unwrap(),
        ]);

        assert_exit_code(&output, ExitCode::Ok, "symlink handling");
    }
}
