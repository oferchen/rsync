/// Returns whether xfer exec commands (early, pre, post) are enabled.
///
/// When the `RSYNC_NO_XFER_EXEC` environment variable is set (to any value),
/// all three exec hooks are suppressed. This allows administrators to disable
/// exec hooks without modifying the daemon configuration.
///
/// upstream: clientserver.c - checks `getenv("RSYNC_NO_XFER_EXEC")` before
/// running any exec hook.
fn xfer_exec_enabled() -> bool {
    std::env::var_os("RSYNC_NO_XFER_EXEC").is_none()
}

/// Information about the current transfer request, used to populate
/// environment variables for pre/post-xfer exec commands.
///
/// Upstream: `clientserver.c` sets `RSYNC_MODULE_NAME`, `RSYNC_MODULE_PATH`,
/// `RSYNC_HOST_ADDR`, `RSYNC_HOST_NAME`, `RSYNC_USER_NAME`, `RSYNC_REQUEST`,
/// `RSYNC_ARG#`, and `RSYNC_PID` before invoking `pre-xfer exec` and
/// `post-xfer exec`.
struct XferExecContext<'a> {
    module_name: &'a str,
    module_path: &'a Path,
    host_addr: IpAddr,
    host_name: Option<&'a str>,
    user_name: Option<&'a str>,
    request: &'a str,
    /// Numbered client arguments for `RSYNC_ARG0`, `RSYNC_ARG1`, etc.
    ///
    /// upstream: clientserver.c:write_pre_exec_args() - sets `RSYNC_ARG<n>`
    /// for each argument the client sent. `RSYNC_ARG0` is typically the
    /// server command name (`rsync`), `RSYNC_ARG1..N` are the remaining args.
    client_args: &'a [String],
}

/// Builds a shell command with the exec-hook environment variables that
/// upstream sets for *every* hook phase (early, pre, and post).
///
/// On Unix, runs via `sh -c <command>`. On Windows, runs via `cmd /C <command>`.
/// This is the shared base: the module/connection identity that upstream sets
/// in the daemon process before forking any hook child. It deliberately omits
/// `RSYNC_REQUEST` and `RSYNC_ARG<n>`, which upstream sets only in the pre-exec
/// child (see `build_pre_xfer_command`).
///
/// upstream: clientserver.c:712/725/726/767/868/903 - `RSYNC_MODULE_NAME`,
/// `RSYNC_HOST_NAME`, `RSYNC_HOST_ADDR`, `RSYNC_USER_NAME`, `RSYNC_MODULE_PATH`,
/// and `RSYNC_PID` are set on the daemon process and thus inherited by both the
/// pre-exec child and the post-xfer parent.
fn build_base_xfer_command(command: &str, ctx: &XferExecContext<'_>) -> ProcessCommand {
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
    cmd.env("RSYNC_HOST_NAME", ctx.host_name.unwrap_or_default());
    cmd.env("RSYNC_USER_NAME", ctx.user_name.unwrap_or_default());
    cmd.env("RSYNC_PID", std::process::id().to_string());

    cmd
}

/// Builds the pre-xfer (and early) exec environment: the shared base plus
/// `RSYNC_REQUEST` and the numbered `RSYNC_ARG<n>` variables.
///
/// upstream: clientserver.c:524/533 - `start_pre_exec()` runs in a forked child
/// that reads the request and argv from a pipe and sets `RSYNC_REQUEST` +
/// `RSYNC_ARG<n>` via `set_env_str`/`set_envN_str`. These live only in the
/// pre-exec child; the post-xfer parent never sees them.
fn build_pre_xfer_command(command: &str, ctx: &XferExecContext<'_>) -> ProcessCommand {
    let mut cmd = build_base_xfer_command(command, ctx);

    cmd.env("RSYNC_REQUEST", ctx.request);

    // upstream: clientserver.c:write_pre_exec_args() - set numbered RSYNC_ARG<n>
    // env vars from the client's argument list.
    for (i, arg) in ctx.client_args.iter().enumerate() {
        cmd.env(format!("RSYNC_ARG{i}"), arg);
    }

    cmd
}

/// Builds the post-xfer exec environment: the shared base only.
///
/// The caller adds `RSYNC_EXIT_STATUS` (and, on Unix, `RSYNC_RAW_STATUS`).
/// upstream: clientserver.c:915-930 - the post-xfer parent sets only the shared
/// env plus the status variables; it never sets `RSYNC_REQUEST` or
/// `RSYNC_ARG<n>`, so a post-xfer hook must not see them.
fn build_post_xfer_command(command: &str, ctx: &XferExecContext<'_>) -> ProcessCommand {
    build_base_xfer_command(command, ctx)
}

/// Runs the early exec command for a daemon module.
///
/// The command is executed via `sh -c` (Unix) or `cmd /C` (Windows) with
/// upstream-compatible environment variables. If the command exits non-zero,
/// returns an error indicating the connection should be denied.
///
/// Upstream: `clientserver.c` - `early_exec()` runs early in the connection,
/// before authentication and argument exchange.
fn run_early_exec(command: &str, ctx: &XferExecContext<'_>) -> io::Result<Result<(), String>> {
    let mut cmd = build_pre_xfer_command(command, ctx);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let output = cmd.output()?;

    if output.status.success() {
        Ok(Ok(()))
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();

        let message = if stderr_trimmed.is_empty() {
            format!(
                "early exec command failed with exit code {code} for module '{}'",
                ctx.module_name
            )
        } else {
            format!(
                "early exec command failed with exit code {code} for module '{}': {stderr_trimmed}",
                ctx.module_name
            )
        };

        Ok(Err(message))
    }
}

/// Result of a successful pre-xfer exec invocation.
///
/// Carries captured stdout from the script so the caller can relay it to the
/// client as an informational message before the transfer begins.
///
/// upstream: clientserver.c:pre_exec() - stdout from the script is sent to the
/// client via `rprintf(FINFO, ...)`.
#[derive(Debug)]
struct PreXferOutput {
    /// Captured stdout from the pre-xfer exec script, trimmed of trailing
    /// whitespace. Empty when the script produced no output.
    stdout: String,
}

/// Error from a failed pre-xfer exec invocation.
///
/// Carries both the error message (for the `@ERROR` response) and any captured
/// stdout (to relay to the client before the error).
#[derive(Debug)]
struct PreXferError {
    /// Human-readable error description including exit code and module name.
    message: String,
    /// Captured stdout from the script, trimmed. Sent to the client before the
    /// `@ERROR` line, matching upstream behaviour.
    stdout: String,
}

/// Runs the pre-xfer exec command for a daemon module.
///
/// The command is executed via `sh -c` (Unix) or `cmd /C` (Windows) with
/// upstream-compatible environment variables. If the command exits non-zero,
/// returns an error indicating the transfer should be denied.
///
/// Stdout from the script is captured and returned in both the success and
/// error paths. The caller is responsible for sending it to the client as an
/// info message (on success) or before the `@ERROR` response (on failure).
///
/// When `early_input` is `Some`, the data is written to the child process's
/// stdin before closing it. This mirrors upstream `clientserver.c:583-584`
/// where `write_buf(write_fd, early_input, early_input_len)` pipes client-sent
/// early-input data to the pre-xfer exec script.
///
/// Upstream: `clientserver.c` - `pre_exec()` / `write_pre_exec_args()` runs
/// the command, captures stdout for the client, and pipes early-input data to
/// its stdin.
fn run_pre_xfer_exec(
    command: &str,
    ctx: &XferExecContext<'_>,
    early_input: Option<&[u8]>,
) -> io::Result<Result<PreXferOutput, PreXferError>> {
    let mut cmd = build_pre_xfer_command(command, ctx);

    if early_input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    // upstream: clientserver.c:pre_exec() - stdout is captured and relayed to
    // the client as an informational message.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    // Pipe early-input data to the child's stdin, then close it.
    // upstream: clientserver.c:583-584 - write_buf(write_fd, early_input, early_input_len)
    if let Some(data) = early_input {
        if let Some(mut stdin) = child.stdin.take() {
            // Best-effort write; ignore broken pipe if the child exits early.
            let _ = stdin.write_all(data);
            drop(stdin);
        }
    }

    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if output.status.success() {
        Ok(Ok(PreXferOutput { stdout }))
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();

        let message = if stderr_trimmed.is_empty() {
            format!(
                "pre-xfer exec returned {code} for module '{}'",
                ctx.module_name
            )
        } else {
            format!(
                "pre-xfer exec returned {code} for module '{}': {stderr_trimmed}",
                ctx.module_name
            )
        };

        Ok(Err(PreXferError { message, stdout }))
    }
}

/// Runs the post-xfer exec command for a daemon module.
///
/// Same environment variables as `run_pre_xfer_exec` plus, on Unix,
/// `RSYNC_RAW_STATUS` (the raw wait-status encoding) and `RSYNC_EXIT_STATUS`
/// (the cooked exit code). Failures are logged but do not change the transfer
/// exit status.
///
/// upstream: clientserver.c:922-927 - the parent that forks the daemon waits
/// for the child, then sets `RSYNC_RAW_STATUS` to the raw `wait_process()`
/// result (which encodes both the exit code and any terminating signal) before
/// cooking it via `WIFEXITED`/`WEXITSTATUS` into `RSYNC_EXIT_STATUS` (or -1 when
/// the child was killed by a signal). oc runs the transfer in-process rather
/// than forking, so the transfer always completes as a normal exit (a signal
/// death is never observable at this point) and `exit_status` is already the
/// cooked code. The raw wait encoding of a normally-exiting process with code
/// `N` is `N << 8`, which decodes back to `N` via `WEXITSTATUS`; mirroring that
/// keeps `$RSYNC_RAW_STATUS` consistent with `$RSYNC_EXIT_STATUS` for hooks that
/// read it. The raw encoding is a POSIX wait-status concept, so it is Unix-only.
///
/// Upstream: `clientserver.c` - `post_exec()` runs the command after the
/// transfer completes, regardless of success or failure.
fn run_post_xfer_exec(
    command: &str,
    ctx: &XferExecContext<'_>,
    exit_status: i32,
    log_sink: Option<&SharedLogSink>,
) {
    let mut cmd = build_post_xfer_command(command, ctx);
    // upstream: clientserver.c:922 - set_env_num("RSYNC_RAW_STATUS", status)
    // is emitted before the cooked RSYNC_EXIT_STATUS; preserve that ordering.
    #[cfg(unix)]
    cmd.env("RSYNC_RAW_STATUS", (exit_status << 8).to_string());
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

    const TEST_ARGS: [String; 0] = [];

    fn test_context() -> XferExecContext<'static> {
        XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
            client_args: &TEST_ARGS,
        }
    }

    #[test]
    fn build_xfer_command_sets_environment_variables() {
        let ctx = test_context();
        let cmd = build_pre_xfer_command("echo test", &ctx);
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
        assert_eq!(find_env("RSYNC_REQUEST").as_deref(), Some("testmod/subdir"));

        let pid_str = find_env("RSYNC_PID");
        assert!(pid_str.is_some(), "RSYNC_PID should be set");
        let pid: u32 = pid_str
            .unwrap()
            .parse()
            .expect("RSYNC_PID should be a valid u32");
        assert_eq!(pid, std::process::id());
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
            client_args: &TEST_ARGS,
        };
        let cmd = build_pre_xfer_command("echo test", &ctx);
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
        let cmd = build_pre_xfer_command("echo hello", &ctx);
        assert_eq!(cmd.get_program(), "sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["-c", "echo hello"]);
    }

    #[cfg(windows)]
    #[test]
    fn build_xfer_command_uses_cmd_on_windows() {
        let ctx = test_context();
        let cmd = build_pre_xfer_command("echo hello", &ctx);
        assert_eq!(cmd.get_program(), "cmd");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["/C", "echo hello"]);
    }

    #[cfg(unix)]
    #[test]
    fn run_early_exec_succeeds_on_zero_exit() {
        let ctx = test_context();
        let result = run_early_exec("true", &ctx).expect("command should run");
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn run_early_exec_fails_on_nonzero_exit() {
        let ctx = test_context();
        let result = run_early_exec("false", &ctx).expect("command should run");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("early exec command failed"));
        assert!(msg.contains("testmod"));
    }

    #[cfg(unix)]
    #[test]
    fn run_early_exec_captures_stderr() {
        let ctx = test_context();
        let result =
            run_early_exec("echo 'early error' >&2; exit 1", &ctx).expect("command should run");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("early error"));
    }

    #[cfg(unix)]
    #[test]
    fn run_early_exec_passes_env_variables() {
        let ctx = test_context();
        let result = run_early_exec(
            "test \"$RSYNC_MODULE_NAME\" = \"testmod\" && test \"$RSYNC_HOST_ADDR\" = \"192.168.1.100\"",
            &ctx,
        )
        .expect("command should run");
        assert!(result.is_ok(), "env vars should be set correctly");
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
        let err = result.unwrap_err();
        assert!(err.message.contains("pre-xfer exec returned"));
        assert!(err.message.contains("testmod"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_captures_stderr() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("echo 'custom error' >&2; exit 1", &ctx, None)
            .expect("command should run");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("custom error"));
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
        run_post_xfer_exec("test \"$RSYNC_EXIT_STATUS\" = \"42\"", &ctx, 42, None);
    }

    #[cfg(unix)]
    #[test]
    fn run_post_xfer_exec_sets_raw_status_consistent_with_exit_status() {
        // upstream: clientserver.c:922-927 - RSYNC_RAW_STATUS holds the raw
        // wait-status; RSYNC_EXIT_STATUS is the cooked code. For a normal exit
        // with code N, the raw wait encoding is N<<8 and WEXITSTATUS(raw) == N.
        // The hook writes both env vars to a file so the assertions can fail;
        // run_post_xfer_exec deliberately swallows the hook's own exit status,
        // so an in-shell `test` could not surface a regression.
        let ctx = test_context();
        for cooked in [0i32, 23] {
            let dir = tempfile::tempdir().unwrap();
            let out_path = dir.path().join("status_env.txt");
            let cmd = format!(
                "printf '%s\\n%s\\n' \"$RSYNC_RAW_STATUS\" \"$RSYNC_EXIT_STATUS\" > '{}'",
                out_path.display()
            );
            run_post_xfer_exec(&cmd, &ctx, cooked, None);

            let contents = std::fs::read_to_string(&out_path).expect("env file should exist");
            let mut lines = contents.lines();
            let raw: i32 = lines
                .next()
                .expect("RSYNC_RAW_STATUS line")
                .parse()
                .expect("RSYNC_RAW_STATUS should be numeric");
            let exit: i32 = lines
                .next()
                .expect("RSYNC_EXIT_STATUS line")
                .parse()
                .expect("RSYNC_EXIT_STATUS should be numeric");

            assert_eq!(exit, cooked, "cooked exit status should be passed through");
            assert_eq!(raw, cooked << 8, "raw status should be the wait encoding");
            // The raw value must decode back to the cooked code via WEXITSTATUS.
            assert_eq!((raw >> 8) & 0xff, cooked, "WEXITSTATUS(raw) must equal cooked");
        }
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
        let result =
            run_pre_xfer_exec("/nonexistent/binary/path", &ctx, None).expect("sh -c should run");
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
        let result = run_pre_xfer_exec(&cmd, &ctx, Some(data)).expect("command should run");
        assert!(result.is_ok(), "script should exit 0");
        let captured = std::fs::read(&out_path).expect("output file should exist");
        assert_eq!(captured, data);
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_without_early_input_has_closed_stdin() {
        let ctx = test_context();
        // `cat` with null stdin exits 0 immediately.
        let result = run_pre_xfer_exec("cat", &ctx, None).expect("command should run");
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
        let result = run_pre_xfer_exec(&cmd, &ctx, Some(&data)).expect("command should run");
        assert!(result.is_ok());
        let captured = std::fs::read(&out_path).expect("output file should exist");
        assert_eq!(captured, data);
    }

    #[test]
    fn xfer_exec_enabled_returns_true_when_env_unset() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::test_env::EnvGuard::remove("RSYNC_NO_XFER_EXEC");
        assert!(xfer_exec_enabled());
    }

    #[test]
    fn xfer_exec_enabled_returns_false_when_env_set() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard =
            crate::test_env::EnvGuard::set("RSYNC_NO_XFER_EXEC", std::ffi::OsStr::new("1"));
        assert!(!xfer_exec_enabled());
    }

    #[test]
    fn xfer_exec_enabled_returns_false_for_empty_value() {
        let _lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = crate::test_env::EnvGuard::set("RSYNC_NO_XFER_EXEC", std::ffi::OsStr::new(""));
        assert!(!xfer_exec_enabled());
    }

    #[test]
    fn build_xfer_command_sets_rsync_arg_env_vars() {
        let args = vec![
            "rsync".to_string(),
            "--server".to_string(),
            "--sender".to_string(),
            "-vlogDtpr".to_string(),
            ".".to_string(),
            "testmod/".to_string(),
        ];
        let ctx = XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
            client_args: &args,
        };
        let cmd = build_pre_xfer_command("echo test", &ctx);
        let envs: Vec<_> = cmd.get_envs().collect();

        let find_env = |key: &str| -> Option<String> {
            envs.iter()
                .find(|(k, _)| k == &key)
                .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()))
        };

        assert_eq!(find_env("RSYNC_ARG0").as_deref(), Some("rsync"));
        assert_eq!(find_env("RSYNC_ARG1").as_deref(), Some("--server"));
        assert_eq!(find_env("RSYNC_ARG2").as_deref(), Some("--sender"));
        assert_eq!(find_env("RSYNC_ARG3").as_deref(), Some("-vlogDtpr"));
        assert_eq!(find_env("RSYNC_ARG4").as_deref(), Some("."));
        assert_eq!(find_env("RSYNC_ARG5").as_deref(), Some("testmod/"));
        assert!(find_env("RSYNC_ARG6").is_none());
    }

    #[test]
    fn build_xfer_command_no_rsync_arg_vars_with_empty_args() {
        let args: Vec<String> = vec![];
        let ctx = XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
            client_args: &args,
        };
        let cmd = build_pre_xfer_command("echo test", &ctx);
        let envs: Vec<_> = cmd.get_envs().collect();

        let find_env = |key: &str| -> Option<String> {
            envs.iter()
                .find(|(k, _)| k == &key)
                .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()))
        };

        assert!(find_env("RSYNC_ARG0").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_captures_stdout_on_success() {
        let ctx = test_context();
        let result =
            run_pre_xfer_exec("echo 'hello from script'", &ctx, None).expect("command should run");
        let output = result.expect("should succeed");
        assert_eq!(output.stdout, "hello from script");
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_captures_stdout_on_failure() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("echo 'pre-xfer info'; exit 1", &ctx, None)
            .expect("command should run");
        let err = result.unwrap_err();
        assert_eq!(err.stdout, "pre-xfer info");
        assert!(err.message.contains("pre-xfer exec returned"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_empty_stdout_on_success() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("true", &ctx, None).expect("command should run");
        let output = result.expect("should succeed");
        assert!(output.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_multiline_stdout() {
        let ctx = test_context();
        let result = run_pre_xfer_exec("echo 'line1'; echo 'line2'; echo 'line3'", &ctx, None)
            .expect("command should run");
        let output = result.expect("should succeed");
        assert!(output.stdout.contains("line1"));
        assert!(output.stdout.contains("line2"));
        assert!(output.stdout.contains("line3"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_xfer_exec_rsync_arg_env_vars_available() {
        let args = vec!["rsync".to_string(), "--server".to_string()];
        let ctx = XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
            client_args: &args,
        };
        let result = run_pre_xfer_exec(
            "test \"$RSYNC_ARG0\" = \"rsync\" && test \"$RSYNC_ARG1\" = \"--server\"",
            &ctx,
            None,
        )
        .expect("command should run");
        assert!(result.is_ok(), "RSYNC_ARG env vars should be set correctly");
    }

    /// Pins the pre- vs post-xfer environment split. upstream sets
    /// `RSYNC_REQUEST` + `RSYNC_ARG<n>` only in the pre-exec child
    /// (clientserver.c:524/533); the post-xfer parent (clientserver.c:915-930)
    /// exports neither. The pre-xfer env must carry them and the post-xfer env
    /// must not, while both share the module/connection identity base.
    #[test]
    fn pre_xfer_env_has_argv_request_and_post_xfer_env_does_not() {
        let args = vec![
            "rsync".to_string(),
            "--server".to_string(),
            "--sender".to_string(),
        ];
        let ctx = XferExecContext {
            module_name: "testmod",
            module_path: Path::new("/srv/testmod"),
            host_addr: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            host_name: Some("client.example.com"),
            user_name: Some("testuser"),
            request: "testmod/subdir",
            client_args: &args,
        };

        let collect = |cmd: &ProcessCommand| -> Vec<String> {
            cmd.get_envs()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect()
        };

        let pre_env = collect(&build_pre_xfer_command("echo test", &ctx));
        let post_env = collect(&build_post_xfer_command("echo test", &ctx));

        // The shared identity base must appear in both phases.
        for shared in [
            "RSYNC_MODULE_NAME",
            "RSYNC_MODULE_PATH",
            "RSYNC_HOST_ADDR",
            "RSYNC_HOST_NAME",
            "RSYNC_USER_NAME",
            "RSYNC_PID",
        ] {
            assert!(pre_env.iter().any(|k| k == shared), "pre missing {shared}");
            assert!(
                post_env.iter().any(|k| k == shared),
                "post missing {shared}"
            );
        }

        // RSYNC_REQUEST and every RSYNC_ARG<n> belong to the pre-xfer child only.
        assert!(pre_env.iter().any(|k| k == "RSYNC_REQUEST"));
        assert!(pre_env.iter().any(|k| k == "RSYNC_ARG0"));
        assert!(pre_env.iter().any(|k| k == "RSYNC_ARG2"));

        assert!(
            !post_env.iter().any(|k| k == "RSYNC_REQUEST"),
            "post-xfer env must not leak RSYNC_REQUEST (upstream sets it only in the pre-exec child)",
        );
        assert!(
            !post_env.iter().any(|k| k.starts_with("RSYNC_ARG")),
            "post-xfer env must not leak any RSYNC_ARG<n> (upstream sets them only in the pre-exec child)",
        );
    }
}
