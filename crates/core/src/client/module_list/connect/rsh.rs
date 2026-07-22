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
//! upstream: `main.c:603-613` + `main.c:1593-1608` - the daemon-over-rsh path
//! in `start_client()` runs `rsync_path --server --daemon .` with no
//! `server_options()`, exports `RSYNC_PORT` into the shell environment, and
//! then speaks the daemon protocol over the spawned process's stdin/stdout.

use std::ffi::{OsStr, OsString};
use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::DaemonStream;
use crate::client::AddressMode;
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
    /// Forced address family (`--ipv4`/`--ipv6`), appended as `-4`/`-6` when
    /// the remote-shell program is exactly `ssh`.
    pub address_mode: AddressMode,
}

/// Returns whether the remote-shell program is ssh (or an ssh-family binary
/// such as `/usr/bin/ssh`), which understands the `-o`/`-J` connection
/// options. A custom `-e` program (`rsh`, the testsuite `lsh.sh`, etc.) does
/// not, and upstream only injects those SSH flags when the rsh is ssh. `-l
/// <user>` is unaffected by this gate: upstream emits it for any program.
fn is_ssh_like(program: &OsStr) -> bool {
    let name = std::path::Path::new(program)
        .file_name()
        .map_or_else(|| program.to_string_lossy(), |n| n.to_string_lossy());
    name == "ssh" || name.ends_with("ssh")
}

/// Returns whether the remote-shell program basename is exactly `ssh` (or
/// `ssh.exe`). Unlike [`is_ssh_like`], this rejects ssh-family wrappers such as
/// `autossh`/`hpnssh`, matching upstream's `strcmp(t, "ssh") == 0` gate for the
/// `-4`/`-6` append and `rsync_io`'s `SshCommand::is_ssh_program`.
fn is_ssh_exact(program: &OsStr) -> bool {
    let name = std::path::Path::new(program)
        .file_name()
        .map_or_else(|| program.to_string_lossy(), |n| n.to_string_lossy());
    name == "ssh" || name == "ssh.exe"
}

/// Builds the `(program, argv)` pair for the daemon-over-rsh spawn without
/// touching the environment or spawning a process, so the argv layout can be
/// unit-tested deterministically.
fn build_rsh_command_argv(spec: &RshDaemonSpawn<'_>) -> (OsString, Vec<OsString>) {
    let ssh_program = if spec.shell_args.is_empty() {
        OsStr::new("ssh")
    } else {
        spec.shell_args[0].as_os_str()
    };

    let mut args: Vec<OsString> = Vec::new();
    for opt in spec.shell_args.iter().skip(1) {
        args.push(opt.clone());
    }

    // upstream: main.c - the `-o`/`-J` connection options are SSH-specific and
    // are only injected when the remote-shell program is ssh. A custom
    // `-e PROG` (e.g. the testsuite `lsh.sh`, or `rsh`) is spawned verbatim as
    // `PROG <pre-args> <host> "<cmd>"`; passing it `-o ConnectTimeout=...`
    // breaks programs that do not understand SSH flags (lsh.sh reads the next
    // token as the host and fails with "unable to connect to host
    // ConnectTimeout=10").
    let ssh_like = is_ssh_like(ssh_program);
    if ssh_like {
        if let Some(bind_addr) = spec.bind_address {
            args.push(OsString::from("-o"));
            args.push(OsString::from(format!("BindAddress={bind_addr}")));
        }

        if let Some(jump) = spec.jump_hosts {
            args.push(OsString::from("-J"));
            args.push(jump.to_os_string());
        }

        if let Some(timeout) = spec.connect_timeout {
            args.push(OsString::from("-o"));
            args.push(OsString::from(format!(
                "ConnectTimeout={}",
                timeout.as_secs()
            )));
        }
    }

    // upstream: main.c:569-586 do_cmd() - `-l user` is emitted as a separate
    // argument for ANY remote-shell program, not just ssh: `lsh.sh` and
    // traditional `rsh` both parse `-l USER` (see `support/lsh.sh`), so
    // rendering `user@host` instead would leave those wrappers unable to
    // find the host operand. The only case upstream skips re-emitting the
    // parsed user is `daemon_connection && dash_l_set` - the caller already
    // wrote a literal `-l <user>` into the `--rsh`/`-e` string:
    // `for (i = 0; i < argc-1; i++) if (!strcmp(args[i], "-l") &&
    // args[i+1][0] != '-') dash_l_set = 1;` (main.c:569-573), scanning the
    // whole tokenized `--rsh` argv. This path is always a daemon connection
    // (daemon-over-rsh), so the condition reduces to `!dash_l_set`.
    let dash_l_set = spec
        .shell_args
        .windows(2)
        .any(|w| w[0].to_string_lossy() == "-l" && !w[1].to_string_lossy().starts_with('-'));
    if let Some(user) = spec.username
        && !dash_l_set
    {
        args.push(OsString::from("-l"));
        args.push(OsString::from(user));
    }

    // upstream: main.c:588-593 do_cmd() - -4/-6 appended when default_af_hint
    // set && strcmp(t,"ssh")==0, before the daemon_connection branch (applies
    // to daemon-over-rsh too). Placed immediately before the host operand,
    // after any `-l user`. Gated on the exact `ssh` basename, so ssh-family
    // wrappers (autossh) and non-ssh shells (rsh) are left untouched.
    if is_ssh_exact(ssh_program) {
        match spec.address_mode {
            AddressMode::Ipv4 => args.push(OsString::from("-4")),
            AddressMode::Ipv6 => args.push(OsString::from("-6")),
            AddressMode::Default => {}
        }
    }

    // The login was already rendered above as `-l user`; the operand here is
    // always the bare host (upstream do_cmd() never composes `user@host`).
    args.push(OsString::from(spec.host));

    // upstream: main.c:603-613 - the remote command is
    // `rsync_path --server --daemon .` with no server_options().
    let rsync_path = spec.rsync_path.unwrap_or_else(|| OsStr::new("rsync"));
    args.push(rsync_path.to_os_string());
    args.push(OsString::from("--server"));
    args.push(OsString::from("--daemon"));
    args.push(OsString::from("."));

    (ssh_program.to_os_string(), args)
}

/// Spawns the remote shell and wraps its stdio as a [`DaemonStream`].
///
/// Builds `PROG <pre-args> [ssh opts] <host> "<rsync-path> --server --daemon ."`
/// matching upstream `main.c`'s daemon-over-rsh invocation, then returns a
/// stream carrying the `@RSYNCD:` protocol over the child's pipes.
pub(crate) fn spawn_rsh_daemon_stream(
    spec: RshDaemonSpawn<'_>,
) -> Result<DaemonStream, ClientError> {
    let (program, args) = build_rsh_command_argv(&spec);
    let mut cmd = Command::new(&program);
    cmd.args(&args);

    // upstream: main.c:1593-1594 - set_env_num("RSYNC_PORT", env_port)
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
            address_mode: AddressMode::Default,
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

    fn argv_strings(shell: &str, mode: AddressMode) -> Vec<String> {
        let shell_args = vec![OsString::from(shell)];
        let spec = RshDaemonSpawn {
            shell_args: &shell_args,
            host: "example.com",
            username: None,
            port: 873,
            rsync_path: None,
            bind_address: None,
            jump_hosts: None,
            connect_timeout: None,
            address_mode: mode,
        };
        let (_, args) = build_rsh_command_argv(&spec);
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// `--ipv4` over daemon-over-ssh must append `-4` immediately before the
    /// host operand, matching upstream do_cmd() which runs the family append
    /// before the `daemon_connection` branch. upstream: main.c:588-589.
    #[test]
    fn appends_ipv4_flag_before_host_for_ssh() {
        let rendered = argv_strings("ssh", AddressMode::Ipv4);
        let flag = rendered.iter().position(|a| a == "-4");
        let host = rendered.iter().position(|a| a == "example.com");
        assert!(
            rendered.contains(&"-4".to_owned()),
            "expected -4 in {rendered:?}"
        );
        assert!(
            !rendered.contains(&"-6".to_owned()),
            "unexpected -6 in {rendered:?}"
        );
        assert_eq!(
            flag.zip(host).map(|(f, h)| f + 1 == h),
            Some(true),
            "-4 must sit immediately before the host: {rendered:?}"
        );
    }

    /// upstream: main.c:592-593 - `--ipv6` appends `-6`.
    #[test]
    fn appends_ipv6_flag_for_ssh() {
        let rendered = argv_strings("ssh", AddressMode::Ipv6);
        assert!(
            rendered.contains(&"-6".to_owned()),
            "expected -6 in {rendered:?}"
        );
        assert!(
            !rendered.contains(&"-4".to_owned()),
            "unexpected -4 in {rendered:?}"
        );
    }

    /// Default address mode injects neither flag; upstream gates the append on
    /// `default_af_hint` being set. upstream: main.c:588/592.
    #[test]
    fn omits_family_flag_for_default_mode() {
        let rendered = argv_strings("ssh", AddressMode::Default);
        assert!(
            !rendered.contains(&"-4".to_owned()),
            "unexpected -4 in {rendered:?}"
        );
        assert!(
            !rendered.contains(&"-6".to_owned()),
            "unexpected -6 in {rendered:?}"
        );
    }

    /// The gate is exact-`ssh`: an ssh-family wrapper like `autossh` (which
    /// `ends_with("ssh")`) must NOT receive `-4`, proving we match upstream's
    /// `strcmp(t,"ssh")==0` rather than a suffix test. upstream: main.c:588.
    #[test]
    fn does_not_append_family_flag_for_autossh() {
        let rendered = argv_strings("autossh", AddressMode::Ipv4);
        assert!(
            !rendered.contains(&"-4".to_owned()),
            "unexpected -4 for autossh: {rendered:?}"
        );
        assert!(
            !rendered.contains(&"-6".to_owned()),
            "unexpected -6 for autossh: {rendered:?}"
        );
    }

    /// A non-ssh custom shell (`rsh`) never receives `-4`/`-6`.
    /// upstream: main.c:588/592 gate on `strcmp(t,"ssh")==0`.
    #[test]
    fn does_not_append_family_flag_for_rsh() {
        let rendered = argv_strings("rsh", AddressMode::Ipv4);
        assert!(
            !rendered.contains(&"-4".to_owned()),
            "unexpected -4 for rsh: {rendered:?}"
        );
        assert!(
            !rendered.contains(&"-6".to_owned()),
            "unexpected -6 for rsh: {rendered:?}"
        );
    }

    fn argv_strings_with_user(shell: &str, username: Option<&str>) -> Vec<String> {
        let shell_args = vec![OsString::from(shell)];
        let spec = RshDaemonSpawn {
            shell_args: &shell_args,
            host: "example.com",
            username,
            port: 873,
            rsync_path: None,
            bind_address: None,
            jump_hosts: None,
            connect_timeout: None,
            address_mode: AddressMode::Default,
        };
        let (_, args) = build_rsh_command_argv(&spec);
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// A username parsed from `user@host::module` must render as a separate
    /// `-l user` argument ahead of the bare host, even for a non-ssh custom
    /// `-e` program. WHY: `support/lsh.sh` in the upstream testsuite only
    /// recognises `-l USER` and matches the literal hostnames `localhost`/
    /// `lh`; a `user@localhost` operand would fall through to its `*)`
    /// branch and fail with "unable to connect to host user@localhost",
    /// silently breaking every `--rsh`/`-e` script written to the
    /// traditional `rsh -l user host` calling convention.
    /// upstream: main.c:569-586 do_cmd() emits `-l user` unconditionally.
    #[test]
    fn renders_dash_l_user_for_non_ssh_program() {
        let rendered = argv_strings_with_user("lsh.sh", Some("backup"));
        let l_idx = rendered.iter().position(|a| a == "-l");
        let user_idx = rendered.iter().position(|a| a == "backup");
        let host_idx = rendered.iter().position(|a| a == "example.com");
        assert_eq!(
            l_idx.map(|i| i + 1),
            user_idx,
            "-l must immediately precede the user: {rendered:?}"
        );
        assert!(
            user_idx.zip(host_idx).is_some_and(|(u, h)| u < h),
            "-l user must precede the host operand: {rendered:?}"
        );
        assert!(
            !rendered.iter().any(|a| a.contains('@')),
            "no user@host composite should be rendered: {rendered:?}"
        );
    }

    /// The same `-l user` placement applies to `ssh` itself.
    #[test]
    fn renders_dash_l_user_for_ssh() {
        let rendered = argv_strings_with_user("ssh", Some("backup"));
        assert_eq!(
            rendered,
            vec![
                "-l".to_owned(),
                "backup".to_owned(),
                "example.com".to_owned(),
                "rsync".to_owned(),
                "--server".to_owned(),
                "--daemon".to_owned(),
                ".".to_owned(),
            ]
        );
    }

    /// No username means no `-l` is emitted at all.
    #[test]
    fn omits_dash_l_without_username() {
        let rendered = argv_strings_with_user("ssh", None);
        assert!(
            !rendered.contains(&"-l".to_owned()),
            "unexpected -l with no username: {rendered:?}"
        );
    }

    /// When the caller's `--rsh` string already spells out a literal
    /// `-l <user>` (the `dash_l_set` case), the parsed `user@host` login must
    /// not be re-emitted as a second `-l`, matching upstream's
    /// `daemon_connection && dash_l_set` skip.
    /// upstream: main.c:569-573,583-586.
    #[test]
    fn skips_dash_l_when_already_set_in_rsh_string() {
        let shell_args = vec![
            OsString::from("ssh"),
            OsString::from("-l"),
            OsString::from("preconfigured"),
        ];
        let spec = RshDaemonSpawn {
            shell_args: &shell_args,
            host: "example.com",
            username: Some("backup"),
            port: 873,
            rsync_path: None,
            bind_address: None,
            jump_hosts: None,
            connect_timeout: None,
            address_mode: AddressMode::Default,
        };
        let (_, args) = build_rsh_command_argv(&spec);
        let rendered: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let l_count = rendered.iter().filter(|a| a.as_str() == "-l").count();
        assert_eq!(
            l_count, 1,
            "the pre-existing -l must not be duplicated: {rendered:?}"
        );
        assert!(
            rendered.contains(&"preconfigured".to_owned()),
            "the literal -l value from the --rsh string must survive: {rendered:?}"
        );
        assert!(
            !rendered.contains(&"backup".to_owned()),
            "the parsed user@host login must be suppressed: {rendered:?}"
        );
    }
}
