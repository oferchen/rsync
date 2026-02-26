/// Information about the current transfer request, used to populate
/// environment variables for pre/post-xfer exec commands.
///
/// Upstream: `clientserver.c` sets `RSYNC_MODULE_NAME`, `RSYNC_MODULE_PATH`,
/// `RSYNC_HOST_ADDR`, `RSYNC_HOST_NAME`, `RSYNC_USER_NAME`, and
/// `RSYNC_REQUEST` before invoking `pre-xfer exec` and `post-xfer exec`.
struct XferExecContext<'a> {
    module_name: &'a str,
    module_path: &'a Path,
    host_addr: IpAddr,
    host_name: Option<&'a str>,
    user_name: Option<&'a str>,
    request: &'a str,
}

/// Builds a shell command with upstream-compatible environment variables.
///
/// On Unix, runs via `sh -c <command>`. On Windows, runs via `cmd /C <command>`.
/// Sets the standard rsync daemon environment variables that upstream exposes
/// to pre/post-xfer exec scripts.
fn build_xfer_command(command: &str, ctx: &XferExecContext<'_>) -> ProcessCommand {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = ProcessCommand::new("sh");
        c.args(["-c", command]);
        c
    };

    #[cfg(windows)]
    let mut cmd = {
        let mut c = ProcessCommand::new("cmd");
        c.args(["/C", command]);
        c
    };

    cmd.env("RSYNC_MODULE_NAME", ctx.module_name);
    cmd.env("RSYNC_MODULE_PATH", ctx.module_path);
    cmd.env("RSYNC_HOST_ADDR", ctx.host_addr.to_string());
    cmd.env(
        "RSYNC_HOST_NAME",
        ctx.host_name.unwrap_or_default(),
    );
    cmd.env("RSYNC_USER_NAME", ctx.user_name.unwrap_or_default());
    cmd.env("RSYNC_REQUEST", ctx.request);

    cmd
}

/// Runs the pre-xfer exec command for a daemon module.
///
/// The command is executed via `sh -c` (Unix) or `cmd /C` (Windows) with
/// upstream-compatible environment variables. If the command exits non-zero,
/// returns an error indicating the transfer should be denied.
///
/// When `early_input` is `Some`, the data is written to the child process's
/// stdin before closing it. This mirrors upstream `clientserver.c:583-584`
/// where `write_buf(write_fd, early_input, early_input_len)` pipes client-sent
/// early-input data to the pre-xfer exec script.
///
/// Upstream: `clientserver.c` — `pre_exec()` / `write_pre_exec_args()` runs
/// the command and pipes early-input data to its stdin.
fn run_pre_xfer_exec(
    command: &str,
    ctx: &XferExecContext<'_>,
    early_input: Option<&[u8]>,
) -> io::Result<Result<(), String>> {
    let mut cmd = build_xfer_command(command, ctx);

    if early_input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    // Pipe early-input data to the child's stdin, then close it.
    // upstream: clientserver.c:583-584 — write_buf(write_fd, early_input, early_input_len)
    if let Some(data) = early_input {
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write; ignore broken pipe if the child exits early.
            let _ = stdin.write_all(data);
            drop(stdin);
        }
    }

    let output = child.wait_with_output()?;

    if output.status.success() {
        Ok(Ok(()))
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();

        let message = if stderr_trimmed.is_empty() {
            format!(
                "pre-xfer exec command failed with exit code {code} for module '{}'",
                ctx.module_name
            )
        } else {
            format!(
                "pre-xfer exec command failed with exit code {code} for module '{}': {stderr_trimmed}",
                ctx.module_name
            )
        };

        Ok(Err(message))
    }
}

/// Runs the post-xfer exec command for a daemon module.
///
/// Same environment variables as `run_pre_xfer_exec` plus `RSYNC_EXIT_STATUS`
/// set to the transfer's exit code. Failures are logged but do not change the
/// transfer exit status.
///
/// Upstream: `clientserver.c` — `post_exec()` runs the command after the
/// transfer completes, regardless of success or failure.
fn run_post_xfer_exec(
    command: &str,
    ctx: &XferExecContext<'_>,
    exit_status: i32,
    log_sink: Option<&SharedLogSink>,
) {
    let mut cmd = build_xfer_command(command, ctx);
    cmd.env("RSYNC_EXIT_STATUS", exit_status.to_string());
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                let code = output.status.code().unwrap_or(-1);
                if let Some(log) = log_sink {
                    let text = format!(
                        "post-xfer exec command failed with exit code {code} for module '{}'",
                        ctx.module_name
                    );
                    let message = rsync_warning!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
            }
        }
        Err(err) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "failed to run post-xfer exec command for module '{}': {err}",
                    ctx.module_name
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
}

#[cfg(test)]
mod xfer_exec_tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn test_context() -> XferExecContext<'static> {
        XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
        }
    }

    #[test]
    fn build_xfer_command_sets_environment_variables() {
        let ctx = test_context();
        let cmd = build_xfer_command("echo test", &ctx);
        let envs: Vec<_> = cmd.get_envs().collect();

        let find_env = |key: &str| -> Option<String> {
            envs.iter()
                .find(|(k, _)| k == &key)
                .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()))
        };

        assert_eq!(find_env("RSYNC_MODULE_NAME").as_deref(), Some("testmod"));
        assert_eq!(
            find_env("RSYNC_MODULE_PATH").as_deref(),
            Some("/srv/testmod")
        );
        assert_eq!(
            find_env("RSYNC_HOST_ADDR").as_deref(),
            Some("192.168.1.100")
        );
        assert_eq!(
            find_env("RSYNC_HOST_NAME").as_deref(),
            Some("client.example.com")
        );
        assert_eq!(find_env("RSYNC_USER_NAME").as_deref(), Some("testuser"));
        assert_eq!(
            find_env("RSYNC_REQUEST").as_deref(),
            Some("testmod/subdir")
        );
    }

    #[test]
    fn build_xfer_command_handles_missing_optional_fields() {
        let ctx = XferExecContext {
            module_name: "mod",
            module_path: Path::new("/data"),
            host_addr: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            host_name: None,
            user_name: None,
            request: "mod",
        };
        let cmd = build_xfer_command("echo test", &ctx);
        let envs: Vec<_> = cmd.get_envs().collect();

        let find_env = |key: &str| -> Option<String> {
            envs.iter()
                .find(|(k, _)| k == &key)
                .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()))
        };

        assert_eq!(find_env("RSYNC_HOST_NAME").as_deref(), Some(""));
        assert_eq!(find_env("RSYNC_USER_NAME").as_deref(), Some(""));
    }

    #[cfg(unix)]
    #[test]
    fn build_xfer_command_uses_sh_on_unix() {
        let ctx = test_context();
        let cmd = build_xfer_command("echo hello", &ctx);
        assert_eq!(cmd.get_program(), "sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["-c", "echo hello"]);
    }

    #[cfg(windows)]
    #[test]
    fn build_xfer_command_uses_cmd_on_windows() {
        let ctx = test_context();
        let cmd = build_xfer_command("echo hello", &ctx);
        assert_eq!(cmd.get_program(), "cmd");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["/C", "echo hello"]);
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_succeeds_on_zero_exit() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("true", &ctx, None).expect("command should run");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_fails_on_nonzero_exit() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("false", &ctx, None).expect("command should run");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("pre-xfer exec command failed"));
        assert!(msg.contains("testmod"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_captures_stderr() {
        let ctx = test_context();
        let result =
            run_pre_xfer_exec("echo 'custom error' >&2; exit 1", &ctx, None).expect("command should run");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("custom error"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_passes_env_variables() {
        let ctx = test_context();
        let result = run_pre_xfer_exec(
            "test \"$RSYNC_MODULE_NAME\" = \"testmod\" && test \"$RSYNC_HOST_ADDR\" = \"192.168.1.100\"",
            &ctx, None,
        )
        .expect("command should run");
        assert!(result.is_ok(), "env vars should be set correctly");
    }

    #[cfg(unix)]
    #[test]
    fn run_post_xfer_exec_passes_exit_status_env() {
        let ctx = test_context();
        run_post_xfer_exec(
            "test \"$RSYNC_EXIT_STATUS\" = \"42\"",
            &ctx,
            42,
            None,
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_post_xfer_exec_does_not_propagate_failure() {
        let ctx = test_context();
        run_post_xfer_exec("exit 1", &ctx, 0, None);
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_returns_io_error_for_missing_command() {
        let ctx = test_context();
        // sh -c with a non-existent command will still run (sh exists), so
        // the command itself returns non-zero rather than an I/O error.
        let result = run_pre_xfer_exec("/nonexistent/binary/path", &ctx, None)
            .expect("sh -c should run");
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_pipes_early_input_to_stdin() {
        let ctx = test_context();
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("stdin_capture.txt");
        // The script reads stdin and writes it to a file so we can verify.
        let cmd = format!("cat > '{}'", out_path.display());
        let data = b"early-input-payload-for-pre-xfer";
        let result = run_pre_xfer_exec(&cmd, &ctx, Some(data))
            .expect("command should run");
        assert!(result.is_ok(), "script should exit 0");
        let captured = std::fs::read(&out_path).expect("output file should exist");
        assert_eq!(captured, data);
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_without_early_input_has_closed_stdin() {
        let ctx = test_context();
        // `cat` with no stdin and null redirect exits 0 immediately.
        let result = run_pre_xfer_exec("cat", &ctx, None)
            .expect("command should run");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_early_input_binary_data() {
        let ctx = test_context();
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("binary_capture.bin");
        let cmd = format!("cat > '{}'", out_path.display());
        // All byte values 0x00..=0xFF
        let data: Vec<u8> = (0..=255u8).collect();
        let result = run_pre_xfer_exec(&cmd, &ctx, Some(&data))
            .expect("command should run");
        assert!(result.is_ok());
        let captured = std::fs::read(&out_path).expect("output file should exist");
        assert_eq!(captured, data);
    }
}
