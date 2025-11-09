use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Stdio};

use super::connection::SshConnection;
use super::parse::{RemoteShellParseError, parse_remote_shell};

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
    /// quotes (inside double quotes they only escape `"`, `\\`, `$`, `` ` ``
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
    /// use transport::ssh::SshCommand;
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

        Ok(SshConnection::new(child, Some(stdin), stdout, stderr))
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
    pub(crate) fn command_parts_for_testing(&self) -> (OsString, Vec<OsString>) {
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
        bytes.len() >= 2 && bytes.first() == Some(&b'[') && bytes.last() == Some(&b']')
    }

    #[cfg(not(unix))]
    {
        let text = host.to_string_lossy();
        text.starts_with('[') && text.ends_with(']')
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
