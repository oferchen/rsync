//! Daemon-connection-over-remote-shell transport.
//!
//! Spawns the `-e`/`--rsh` program with `rsync --server --daemon .` as the
//! remote command and wraps its stdio pipes as a [`DaemonStream`], so the
//! client speaks the `@RSYNCD:` daemon protocol over the shell instead of
//! opening a TCP socket to the daemon port.
//!
//! Shared by the transfer path (`remote::run_daemon_over_remote_shell`) and
//! the module-listing path so both honour `-e PROG host::` identically.
//!
//! upstream: `main.c:594-604` + `main.c:1571-1586` - the daemon-over-rsh path
//! in `start_client()` runs `rsync_path --server --daemon .` with no
//! `server_options()`, exports `RSYNC_PORT` into the shell environment, and
//! then speaks the daemon protocol over the spawned process's stdin/stdout.

use std::ffi::{OsStr, OsString};
use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::DaemonStream;
use crate::client::IPC_EXIT_CODE;
use crate::client::error::{ClientError, invalid_argument_error};

/// Parameters for spawning a daemon-connection-over-remote-shell stream.
pub(crate) struct RshDaemonSpawn<'a> {
    /// `-e`/`--rsh` program and its pre-host arguments. `shell_args[0]` is the
    /// program; the remaining entries precede the host on the command line.
    pub shell_args: &'a [OsString],
    /// Daemon host passed to the remote shell.
    pub host: &'a str,
    /// Optional `-l <user>` login name parsed from `user@host::`.
    pub username: Option<&'a str>,
    /// Daemon port, exported as `RSYNC_PORT` for the remote shell.
    pub port: u16,
    /// `--rsync-path` override for the remote command (defaults to `rsync`).
    pub rsync_path: Option<&'a OsStr>,
    /// Optional `-o BindAddress=` applied to the spawned shell.
    pub bind_address: Option<IpAddr>,
    /// Optional `-J` jump-host specification.
    pub jump_hosts: Option<&'a OsStr>,
    /// Optional `-o ConnectTimeout=` in whole seconds.
    pub connect_timeout: Option<Duration>,
}

/// Spawns the remote shell and wraps its stdio as a [`DaemonStream`].
///
/// Builds `PROG <pre-args> [ssh opts] <host> "<rsync-path> --server --daemon ."`
/// matching upstream `main.c`'s daemon-over-rsh invocation, then returns a
/// stream carrying the `@RSYNCD:` protocol over the child's pipes.
pub(crate) fn spawn_rsh_daemon_stream(
    spec: RshDaemonSpawn<'_>,
) -> Result<DaemonStream, ClientError> {
    let ssh_program = if spec.shell_args.is_empty() {
        OsStr::new("ssh")
    } else {
        spec.shell_args[0].as_os_str()
    };
    let mut cmd = Command::new(ssh_program);

    for opt in spec.shell_args.iter().skip(1) {
        cmd.arg(opt);
    }

    if let Some(bind_addr) = spec.bind_address {
        cmd.arg("-o").arg(format!("BindAddress={bind_addr}"));
    }

    if let Some(jump) = spec.jump_hosts {
        cmd.arg("-J").arg(jump);
    }

    if let Some(timeout) = spec.connect_timeout {
        cmd.arg("-o")
            .arg(format!("ConnectTimeout={}", timeout.as_secs()));
    }

    if let Some(user) = spec.username {
        cmd.arg("-l").arg(user);
    }

    cmd.arg(spec.host);

    // upstream: main.c:594-604 - the remote command is
    // `rsync_path --server --daemon .` with no server_options().
    let rsync_path = spec.rsync_path.unwrap_or_else(|| OsStr::new("rsync"));
    cmd.arg(rsync_path).arg("--server").arg("--daemon").arg(".");

    // upstream: main.c:1571-1572 - set_env_num("RSYNC_PORT", env_port)
    cmd.env("RSYNC_PORT", spec.port.to_string());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let mut child = cmd.spawn().map_err(|e| {
        invalid_argument_error(
            &format!("failed to spawn remote shell for daemon-over-rsh: {e}"),
            IPC_EXIT_CODE,
        )
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        invalid_argument_error("remote shell process did not expose stdin", IPC_EXIT_CODE)
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        invalid_argument_error("remote shell process did not expose stdout", IPC_EXIT_CODE)
    })?;

    Ok(DaemonStream::from_child_process(child, stdin, stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Documents the precedence wiring: when a remote shell is configured, the
    /// listing/transfer paths must build a daemon-over-rsh invocation that runs
    /// `PROG ... <host> "<rsync-path> --server --daemon ."` instead of opening
    /// a TCP socket. A bogus shell program lets us assert the spawn attempt
    /// targets the program (not the daemon port) without a live daemon.
    ///
    /// WHY: upstream `rsync -e PROG host::` reaches the daemon through the
    /// spawned shell; regressing to TCP yields `connect()` -> ECONNREFUSED
    /// (exit 10), the exact failure this path fixes.
    #[test]
    fn spawn_rsh_daemon_stream_runs_program_not_tcp() {
        let shell_args = vec![OsString::from("/nonexistent/oc-rsync-rsh-daemon-probe-bin")];
        let spec = RshDaemonSpawn {
            shell_args: &shell_args,
            host: "localhost",
            username: None,
            port: 873,
            rsync_path: None,
            bind_address: None,
            jump_hosts: None,
            connect_timeout: None,
        };

        // `DaemonStream` is not `Debug`, so match instead of `expect_err`.
        let err = match spawn_rsh_daemon_stream(spec) {
            Ok(_) => panic!("spawning a nonexistent shell program must fail"),
            Err(e) => e,
        };

        // The failure must come from attempting to exec the remote-shell
        // program, proving the path did not silently fall back to a TCP
        // connect against the daemon port.
        assert_eq!(err.exit_code(), IPC_EXIT_CODE);
        assert!(
            err.to_string().contains("daemon-over-rsh"),
            "error should reference the daemon-over-rsh spawn, got: {err}"
        );
    }
}
