#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The [`SshCommand`] builder provides a thin wrapper around spawning the
//! system `ssh` (or compatible) binary.  The struct follows a builder pattern:
//! callers configure authentication parameters, additional command-line
//! options, and the remote command to execute before requesting a
//! [`SshConnection`].  The resulting connection implements [`Read`] and
//! [`Write`], allowing higher layers to treat the remote shell exactly like any
//! other byte stream when negotiating rsync sessions.
//!
//! # Design
//!
//! - [`SshCommand`] defaults to the `ssh` binary, enabling batch mode by
//!   default so password prompts never block non-interactive invocations.
//! - Builder-style setters expose user/host pairs, port selection, additional
//!   `ssh` options, and remote command arguments without forcing allocations in
//!   hot paths.
//! - [`SshConnection`] owns the spawned child process and forwards read/write
//!   operations to the child's stdout/stdin.  Dropping the connection flushes
//!   and closes the input pipe before reaping the child to avoid process leaks.
//!
//! # Examples
//!
//! Spawn the local SSH client and stream data to a remote rsync daemon.  The
//! example is marked `no_run` because it requires a reachable host.
//!
//! ```no_run
//! use rsync_transport::ssh::SshCommand;
//! use std::io::{Read, Write};
//!
//! let mut command = SshCommand::new("files.example.com");
//! command.set_user("backup");
//! command.push_remote_arg("rsync");
//! command.push_remote_arg("--server");
//! command.push_remote_arg("--sender");
//! command.push_remote_arg(".");
//!
//! let mut connection = command.spawn().expect("spawn ssh");
//! connection
//!     .write_all(b"@RSYNCD: 32.0\n")
//!     .expect("send greeting");
//! connection.flush().expect("flush transport");
//!
//! let mut response = Vec::new();
//! connection
//!     .read_to_end(&mut response)
//!     .expect("read daemon response");
//! ```
//!
//! # See also
//!
//! - [`crate::session`] for the negotiation façade that consumes
//!   [`SshConnection`] streams.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

mod parse;

pub use parse::{RemoteShellParseError, parse_remote_shell};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Builder used to configure and spawn an SSH subprocess.
#[derive(Clone, Debug)]
pub struct SshCommand {
    program: OsString,
    user: Option<OsString>,
    host: OsString,
    port: Option<u16>,
    batch_mode: bool,
    options: Vec<OsString>,
    remote_command: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    target_override: Option<OsString>,
}

impl SshCommand {
    /// Creates a new builder targeting the provided host name or address.
    #[must_use]
    pub fn new(host: impl Into<OsString>) -> Self {
        Self {
            program: OsString::from("ssh"),
            user: None,
            host: host.into(),
            port: None,
            batch_mode: true,
            options: Vec::new(),
            remote_command: Vec::new(),
            envs: Vec::new(),
            target_override: None,
        }
    }

    /// Overrides the program used to spawn the remote shell.
    pub fn set_program<S: Into<OsString>>(&mut self, program: S) -> &mut Self {
        self.program = program.into();
        self
    }

    /// Sets the remote username. When omitted, the system `ssh` default applies.
    pub fn set_user<S: Into<OsString>>(&mut self, user: S) -> &mut Self {
        self.user = Some(user.into());
        self
    }

    /// Specifies the TCP port used when connecting to the remote host.
    pub fn set_port(&mut self, port: u16) -> &mut Self {
        self.port = Some(port);
        self
    }

    /// Enables or disables batch mode (default: enabled).
    pub fn set_batch_mode(&mut self, enabled: bool) -> &mut Self {
        self.batch_mode = enabled;
        self
    }

    /// Appends an additional option that should appear before the target operand.
    pub fn push_option<S: Into<OsString>>(&mut self, option: S) -> &mut Self {
        self.options.push(option.into());
        self
    }

    /// Replaces the remote command executed after connecting to the host.
    pub fn set_remote_command<I, S>(&mut self, command: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.remote_command = command.into_iter().map(Into::into).collect();
        self
    }

    /// Appends a single argument to the remote command sequence.
    pub fn push_remote_arg<S: Into<OsString>>(&mut self, arg: S) -> &mut Self {
        self.remote_command.push(arg.into());
        self
    }

    /// Adds an environment variable passed to the spawned subprocess.
    pub fn env<K: Into<OsString>, V: Into<OsString>>(&mut self, key: K, value: V) -> &mut Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    /// Overrides the computed target argument. This primarily exists for testing
    /// but can be used to support alternate remote shells.
    pub fn set_target_override<S: Into<OsString>>(&mut self, target: Option<S>) -> &mut Self {
        self.target_override = target.map(Into::into);
        self
    }

    /// Replaces the command and options using a remote-shell specification.
    ///
    /// The specification uses the same quoting rules recognised by upstream
    /// rsync's `-e/--rsh` handling: whitespace separates arguments unless it is
    /// protected by single or double quotes, single quotes inhibit all
    /// escaping, and backslashes escape the following byte outside single
    /// quotes (inside double quotes they only escape `"`, `\`, `$`, `` ` ``
    /// and a trailing newline). The resulting sequence replaces the current
    /// program and option list while leaving the target host and remote command
    /// untouched.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteShellParseError`] when the specification is empty or
    /// contains unterminated quotes/escapes.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::ssh::SshCommand;
    /// use std::ffi::OsStr;
    ///
    /// let mut builder = SshCommand::new("files.example.com");
    /// builder
    ///     .configure_remote_shell(OsStr::new("ssh -p 2222 -l backup"))
    ///     .expect("valid remote shell");
    /// // The builder now invokes `ssh -p 2222 -l backup files.example.com ...`.
    /// ```
    pub fn configure_remote_shell(
        &mut self,
        specification: &OsStr,
    ) -> Result<&mut Self, RemoteShellParseError> {
        let mut parts = parse_remote_shell(specification)?;
        debug_assert!(!parts.is_empty(), "parser guarantees at least one element");

        self.program = parts.remove(0);
        self.options = parts;

        Ok(self)
    }

    /// Spawns the configured command and returns a [`SshConnection`].
    pub fn spawn(&self) -> io::Result<SshConnection> {
        let (program, args) = self.command_parts();
        let mut command = Command::new(&program);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.args(args.iter());

        for (key, value) in &self.envs {
            command.env(key, value);
        }

        let mut child = command.spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh command did not expose a writable stdin",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh command did not expose a readable stdout",
            )
        })?;
        let stderr = child.stderr.take();

        Ok(SshConnection {
            child,
            stdin: Some(stdin),
            stdout,
            stderr,
        })
    }

    fn command_parts(&self) -> (OsString, Vec<OsString>) {
        let mut args = Vec::with_capacity(
            2 + self.options.len() + self.remote_command.len() + usize::from(self.port.is_some()),
        );

        if self.batch_mode {
            args.push(OsString::from("-oBatchMode=yes"));
        }

        if let Some(port) = self.port {
            args.push(OsString::from("-p"));
            args.push(OsString::from(port.to_string()));
        }

        args.extend(self.options.iter().cloned());

        if let Some(target) = self.target_argument() {
            if !target.is_empty() {
                args.push(target);
            }
        }

        args.extend(self.remote_command.iter().cloned());

        (self.program.clone(), args)
    }

    fn target_argument(&self) -> Option<OsString> {
        if let Some(target) = &self.target_override {
            return Some(target.clone());
        }

        if self.host.is_empty() && self.user.is_none() {
            return None;
        }

        let mut target = OsString::new();
        if let Some(user) = &self.user {
            target.push(user);
            target.push("@");
        }

        if host_needs_ipv6_brackets(&self.host) {
            target.push("[");
            target.push(&self.host);
            target.push("]");
        } else {
            target.push(&self.host);
        }

        Some(target)
    }

    #[cfg(test)]
    fn command_parts_for_testing(&self) -> (OsString, Vec<OsString>) {
        self.command_parts()
    }
}

fn host_needs_ipv6_brackets(host: &OsStr) -> bool {
    if host.is_empty() {
        return false;
    }

    if host_is_bracketed(host) {
        return false;
    }

    host_contains_colon(host)
}

fn host_is_bracketed(host: &OsStr) -> bool {
    #[cfg(unix)]
    {
        let bytes = host.as_bytes();
        return bytes.len() >= 2 && bytes.first() == Some(&b'[') && bytes.last() == Some(&b']');
    }

    #[cfg(not(unix))]
    {
        let text = host.to_string_lossy();
        return text.starts_with('[') && text.ends_with(']');
    }
}

fn host_contains_colon(host: &OsStr) -> bool {
    #[cfg(unix)]
    {
        host.as_bytes().contains(&b':')
    }

    #[cfg(not(unix))]
    {
        host.to_string_lossy().contains(':')
    }
}

/// Owns an active SSH subprocess and exposes its stdio handles.
pub struct SshConnection {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: Option<ChildStderr>,
}

impl SshConnection {
    /// Returns a mutable reference to the subprocess stderr stream, when available.
    pub fn stderr_mut(&mut self) -> Option<&mut ChildStderr> {
        self.stderr.as_mut()
    }

    /// Transfers ownership of the subprocess stderr stream to the caller.
    ///
    /// This helper complements [`stderr_mut`](Self::stderr_mut) by allowing
    /// higher layers to move the stderr handle into background readers without
    /// keeping the connection borrowed mutably for the lifetime of the stream.
    /// Subsequent calls return `None`, matching the semantics of
    /// [`Option::take`].
    #[must_use]
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// Flushes and closes the stdin pipe, signalling EOF to the subprocess.
    pub fn close_stdin(&mut self) -> io::Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.flush()?;
        }
        Ok(())
    }

    /// Waits for the subprocess to exit, consuming the connection.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        let _ = self.close_stdin();
        self.child.wait()
    }

    /// Attempts to retrieve the subprocess exit status without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Read for SshConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for SshConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin has already been closed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for SshConnection {
    fn drop(&mut self) {
        let _ = self.close_stdin();

        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }

        let _ = self.child.wait();
    }
}
#[cfg(test)]
mod tests {
    use super::{SshCommand, SshConnection};
    use std::ffi::{OsStr, OsString};
    use std::io::{Read, Write};

    fn args_to_strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn assembles_minimal_command_with_batch_mode() {
        let command = SshCommand::new("example.com");
        let (program, args) = command.command_parts_for_testing();

        assert_eq!(program, OsString::from("ssh"));
        assert_eq!(
            args_to_strings(&args),
            vec!["-oBatchMode=yes".to_string(), "example.com".to_string()]
        );
    }

    #[test]
    fn assembles_command_with_user_port_and_remote_args() {
        let mut command = SshCommand::new("rsync.example.com");
        command.set_user("backup");
        command.set_port(2222);
        command.push_option("-vvv");
        command.push_remote_arg("rsync");
        command.push_remote_arg("--server");
        command.push_remote_arg(".");

        let (_, args) = command.command_parts_for_testing();
        let rendered = args_to_strings(&args);

        assert_eq!(
            rendered,
            vec![
                "-oBatchMode=yes".to_string(),
                "-p".to_string(),
                "2222".to_string(),
                "-vvv".to_string(),
                "backup@rsync.example.com".to_string(),
                "rsync".to_string(),
                "--server".to_string(),
                ".".to_string(),
            ]
        );
    }

    #[test]
    fn disables_batch_mode_when_requested() {
        let mut command = SshCommand::new("example.com");
        command.set_batch_mode(false);

        let (_, args) = command.command_parts_for_testing();
        assert_eq!(args_to_strings(&args), vec!["example.com".to_string()]);
    }

    #[test]
    fn wraps_ipv6_hosts_in_brackets() {
        let command = SshCommand::new("2001:db8::1");
        let (_, args) = command.command_parts_for_testing();

        assert_eq!(
            args_to_strings(&args),
            vec!["-oBatchMode=yes".to_string(), "[2001:db8::1]".to_string()]
        );
    }

    #[test]
    fn wraps_ipv6_hosts_with_usernames() {
        let mut command = SshCommand::new("2001:db8::1");
        command.set_user("backup");

        let (_, args) = command.command_parts_for_testing();

        assert_eq!(
            args_to_strings(&args),
            vec![
                "-oBatchMode=yes".to_string(),
                "backup@[2001:db8::1]".to_string()
            ]
        );
    }

    #[test]
    fn preserves_explicit_bracketed_ipv6_literals() {
        let mut command = SshCommand::new("[2001:db8::1]");
        command.set_user("backup");

        let (_, args) = command.command_parts_for_testing();

        assert_eq!(
            args_to_strings(&args),
            vec![
                "-oBatchMode=yes".to_string(),
                "backup@[2001:db8::1]".to_string()
            ]
        );
    }

    #[cfg(unix)]
    fn spawn_echo_process() -> SshConnection {
        let mut command = SshCommand::new("ignored");
        command.set_program("sh");
        command.set_batch_mode(false);
        command.push_option("-c");
        command.push_option("cat");

        command
            .spawn()
            .expect("failed to spawn local shell for testing")
    }

    #[cfg(unix)]
    #[test]
    fn spawned_connection_forwards_io() {
        let mut connection = spawn_echo_process();

        connection.write_all(b"abc").expect("write payload");
        connection.flush().expect("flush payload");

        let mut buffer = [0u8; 3];
        connection.read_exact(&mut buffer).expect("read echo");
        assert_eq!(&buffer, b"abc");

        let status = connection.wait().expect("wait for process");
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn stderr_stream_is_accessible() {
        let mut command = SshCommand::new("ignored");
        command.set_program("sh");
        command.set_batch_mode(false);
        command.push_option("-c");
        command.push_option("printf err >&2");

        let mut connection = command.spawn().expect("spawn shell");
        connection.close_stdin().expect("close stdin");

        let mut stderr = String::new();
        connection
            .stderr_mut()
            .expect("stderr handle")
            .read_to_string(&mut stderr)
            .expect("read stderr");
        assert!(stderr.contains("err"));

        let status = connection.wait().expect("wait status");
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn take_stderr_transfers_handle() {
        let mut command = SshCommand::new("ignored");
        command.set_program("sh");
        command.set_batch_mode(false);
        command.push_option("-c");
        command.push_option("printf err >&2");

        let mut connection = command.spawn().expect("spawn shell");
        connection.close_stdin().expect("close stdin");

        let mut handle = connection.take_stderr().expect("stderr handle");
        assert!(connection.take_stderr().is_none());

        let mut captured = String::new();
        handle.read_to_string(&mut captured).expect("read stderr");
        assert!(captured.contains("err"));

        let status = connection.wait().expect("wait status");
        assert!(status.success());
    }

    #[test]
    fn configure_remote_shell_updates_program_and_options() {
        let mut command = SshCommand::new("example.com");
        command
            .configure_remote_shell(OsStr::new("ssh -p 2222"))
            .expect("configure succeeds");

        let (program, args) = command.command_parts_for_testing();
        assert_eq!(program, OsString::from("ssh"));
        assert!(args.contains(&OsString::from("-p")));
        assert!(args.contains(&OsString::from("2222")));
    }
}
