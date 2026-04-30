use std::ffi::{OsStr, OsString};
use std::io;
use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::aux_channel::{build_stderr_channel, configure_stderr_channel};
use super::connection::SshConnection;
use super::parse::{RemoteShellParseError, parse_remote_shell};
use logging::debug_log;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Default SSH keepalive interval in seconds.
///
/// When keepalive injection is enabled, this value is passed as
/// `-o ServerAliveInterval=N`. The SSH client sends a keepalive message
/// through the encrypted channel after this many seconds of inactivity,
/// preventing idle connections from being dropped by firewalls or NAT
/// devices during long transfers.
const DEFAULT_SERVER_ALIVE_INTERVAL: u32 = 20;

/// Default SSH keepalive retry count.
///
/// When keepalive injection is enabled, this value is passed as
/// `-o ServerAliveCountMax=N`. If the server fails to respond to this
/// many consecutive keepalive messages, SSH terminates the connection.
const DEFAULT_SERVER_ALIVE_COUNT_MAX: u32 = 3;

/// Default SSH connection establishment timeout in seconds.
///
/// When no explicit connect timeout is provided, SSH's TCP handshake is
/// capped at this value. This prevents indefinite hangs when the remote
/// host is unreachable or firewalled. Mirrors upstream rsync's default
/// `--contimeout` behavior.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Builder used to configure and spawn an SSH subprocess.
#[derive(Clone, Debug)]
pub struct SshCommand {
    program: OsString,
    user: Option<OsString>,
    host: OsString,
    port: Option<u16>,
    batch_mode: bool,
    bind_address: Option<IpAddr>,
    keepalive: bool,
    options: Vec<OsString>,
    connect_timeout: Option<Duration>,
    remote_command: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    target_override: Option<OsString>,
    prefer_aes_gcm: Option<bool>,
    jump_hosts: Option<OsString>,
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
            bind_address: None,
            keepalive: true,
            connect_timeout: Some(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS)),
            options: Vec::new(),
            remote_command: Vec::new(),
            envs: Vec::new(),
            target_override: None,
            prefer_aes_gcm: None,
            jump_hosts: None,
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
    pub const fn set_port(&mut self, port: u16) -> &mut Self {
        self.port = Some(port);
        self
    }

    /// Sets the local bind address passed to SSH via `-o BindAddress=<addr>`.
    ///
    /// When specified, SSH will bind its outgoing connection to this address
    /// before connecting to the remote host. This mirrors upstream rsync's
    /// `--address` handling for SSH transports.
    ///
    /// upstream: clientserver.c — `--address` is forwarded to SSH as
    /// `-o BindAddress=<addr>`.
    pub const fn set_bind_address(&mut self, addr: Option<IpAddr>) -> &mut Self {
        self.bind_address = addr;
        self
    }

    /// Enables or disables batch mode (default: enabled).
    pub const fn set_batch_mode(&mut self, enabled: bool) -> &mut Self {
        self.batch_mode = enabled;
        self
    }

    /// Enables or disables SSH keepalive injection (default: enabled).
    ///
    /// When enabled, `-o ServerAliveInterval=20` and
    /// `-o ServerAliveCountMax=3` are injected into the SSH command line,
    /// preventing idle connections from being dropped by firewalls or NAT
    /// devices during long transfers.
    ///
    /// Keepalive options are not injected when:
    /// - The user has already specified `ServerAliveInterval` or
    ///   `ServerAliveCountMax` via `-e` or `push_option`.
    /// - The program is not `ssh` (e.g., `plink`, `rsh`).
    /// - Keepalive is explicitly disabled via `set_keepalive(false)`.
    pub const fn set_keepalive(&mut self, enabled: bool) -> &mut Self {
        self.keepalive = enabled;
        self
    }

    /// Sets the SSH connection establishment timeout.
    ///
    /// When `Some(duration)`, `-o ConnectTimeout=N` is injected into the SSH
    /// command line (where N is the duration in whole seconds, rounded up).
    /// This prevents indefinite hangs when the remote host is unreachable or
    /// firewalled. The option is only injected when the program is `ssh` and
    /// the user has not already specified `ConnectTimeout` via `-o` options.
    ///
    /// When `None`, no connect timeout is injected and SSH uses its own
    /// default (which typically falls through to the OS TCP timeout).
    ///
    /// The default is `Some(Duration::from_secs(30))`.
    pub const fn set_connect_timeout(&mut self, timeout: Option<Duration>) -> &mut Self {
        self.connect_timeout = timeout;
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

    /// Requests AES-GCM cipher selection for the SSH transport.
    ///
    /// When `Some(true)`, injects `-c aes128-gcm@openssh.com,aes256-gcm@openssh.com`
    /// before the target argument, preferring 128-bit for lower per-block
    /// overhead. This is only applied when all of the following hold:
    ///
    /// - The CPU has hardware AES acceleration (AES-NI on x86/x86_64, AES
    ///   instructions on aarch64). Without hardware support, OpenSSH's default
    ///   `chacha20-poly1305@openssh.com` is faster because ChaCha20 is a pure
    ///   software cipher optimized for CPUs lacking AES pipelines.
    /// - The program is `ssh` (or `ssh.exe`). Non-SSH transports such as `rsh`
    ///   or `plink` do not accept `-c`.
    /// - No existing option already specifies `-c`, which would indicate the
    ///   caller (or the user's `-e` remote-shell specification) already controls
    ///   cipher selection.
    ///
    /// When `Some(false)`, cipher injection is explicitly suppressed.
    /// When `None` (the default), no cipher arguments are injected.
    ///
    /// # Performance
    ///
    /// On CPUs with hardware AES, AES-GCM runs in the CPU's AES pipeline and
    /// delivers 2-4x the throughput of software ChaCha20-Poly1305, which is
    /// OpenSSH's default cipher on most distributions. This can materially
    /// improve SSH transfer throughput for large files.
    ///
    /// Upstream rsync does not inject cipher preferences - it relies on OpenSSH
    /// defaults. This is an oc-rsync enhancement.
    pub const fn set_prefer_aes_gcm(&mut self, preference: Option<bool>) -> &mut Self {
        self.prefer_aes_gcm = preference;
        self
    }

    /// Configures the comma-separated list of OpenSSH ProxyJump hosts.
    ///
    /// When `Some(value)` and `value` is non-empty, `-J <value>` is appended
    /// to the SSH argv before the destination operand. The value is forwarded
    /// verbatim and may take the OpenSSH form
    /// `[user@]host[:port][,[user@]host[:port]...]`. An empty `OsString` is
    /// treated as no configuration to avoid emitting a bare `-J ` to ssh.
    ///
    /// `-J` is only injected when the program looks like an SSH client
    /// (`ssh` / `ssh.exe`). Non-SSH transports such as `rsh` or `plink` do
    /// not understand the option and would fail at spawn time.
    pub fn set_jump_hosts<S: Into<OsString>>(&mut self, value: Option<S>) -> &mut Self {
        self.jump_hosts = value.map(Into::into).filter(|v| !v.is_empty());
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
    /// use rsync_io::ssh::SshCommand;
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
    ///
    /// On Unix the child's stderr is wired through a `socketpair(2)` when
    /// possible, exposing a real socket descriptor on the parent side that
    /// future event-loop integrations can poll alongside stdin/stdout. If
    /// socketpair creation fails for any reason (e.g., file-descriptor
    /// exhaustion), the spawn transparently falls back to the conventional
    /// `Stdio::piped()` anonymous pipe. Windows always uses the pipe path.
    pub fn spawn(&self) -> io::Result<SshConnection> {
        let (program, args) = self.command_parts();

        debug_log!(
            Cmd,
            1,
            "spawning ssh: {} {}",
            program.to_string_lossy(),
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );

        let mut command = Command::new(&program);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.args(args.iter());

        for (key, value) in &self.envs {
            command.env(key, value);
        }

        // Attempt to install a socketpair-based stderr channel before
        // spawning. On success, we hold the parent end and configure the
        // command to inherit the child end as its stderr. On failure (or on
        // Windows) we fall back to the conventional anonymous pipe path.
        let parent_socketpair_end = configure_stderr_channel(&mut command);

        let mut child = command.spawn()?;

        debug_log!(Connect, 2, "ssh process spawned successfully");

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

        let stderr_channel = build_stderr_channel(parent_socketpair_end, child.stderr.take());

        Ok(SshConnection::new(
            child,
            Some(stdin),
            stdout,
            stderr_channel,
            self.connect_timeout,
        ))
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

        // Inject bind address before user options so that `-e` overrides work.
        // upstream: rsync passes `--address` to SSH as `-o BindAddress=<addr>`.
        if let Some(addr) = &self.bind_address {
            args.push(OsString::from(format!("-oBindAddress={addr}")));
        }

        // Inject SSH keepalive options to prevent idle connections from being
        // dropped by firewalls or NAT devices during long transfers. Skipped
        // when the user already specifies these options or uses a non-SSH program.
        if self.should_inject_keepalive() {
            args.push(OsString::from(format!(
                "-oServerAliveInterval={DEFAULT_SERVER_ALIVE_INTERVAL}"
            )));
            args.push(OsString::from(format!(
                "-oServerAliveCountMax={DEFAULT_SERVER_ALIVE_COUNT_MAX}"
            )));
        }

        // Inject SSH connect timeout to prevent indefinite hangs when the
        // remote host is unreachable. Skipped when the user already specifies
        // ConnectTimeout or uses a non-SSH program.
        if let Some(seconds) = self.connect_timeout_seconds() {
            if self.should_inject_connect_timeout() {
                args.push(OsString::from(format!("-oConnectTimeout={seconds}")));
            }
        }

        args.extend(self.options.iter().cloned());

        // Inject AES-GCM ciphers when requested and safe to do so.
        // upstream: rsync uses the system SSH default; we optionally prefer
        // hardware-accelerated AES-GCM for throughput on modern CPUs.
        if self.should_inject_aes_gcm_ciphers() {
            args.push(OsString::from("-c"));
            args.push(OsString::from(
                "aes128-gcm@openssh.com,aes256-gcm@openssh.com",
            ));
        }

        // Inject the OpenSSH ProxyJump (`-J`) value when configured and the
        // configured program looks like an SSH client. Mirrors the user's
        // `-J [user@]host[:port][,...]` argument which is forwarded verbatim
        // before the destination operand.
        if let Some(jump) = &self.jump_hosts
            && !jump.is_empty()
            && self.is_ssh_program()
        {
            args.push(OsString::from("-J"));
            args.push(jump.clone());
        }

        if let Some(target) = self.target_argument()
            && !target.is_empty()
        {
            args.push(target);
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

    /// Determines whether AES-GCM cipher arguments should be injected.
    ///
    /// Returns `true` when all of these conditions are met:
    ///
    /// 1. `prefer_aes_gcm` is not `Some(false)` (caller has not opted out).
    /// 2. The CPU has hardware AES - AES-NI on x86/x86_64, or the `aes`
    ///    feature on aarch64 (see [`has_hardware_aes`]).
    /// 3. The program basename is `ssh` or `ssh.exe`.
    /// 4. No existing option already contains `-c` (the user has not specified
    ///    a cipher via `-e "ssh -c ..."` or `push_option`).
    ///
    /// Returns `false` when `prefer_aes_gcm` is `Some(false)` (explicitly
    /// disabled via `--no-aes`) or when any hardware/program/cipher guard
    /// fails.
    fn should_inject_aes_gcm_ciphers(&self) -> bool {
        if matches!(self.prefer_aes_gcm, Some(false)) {
            return false;
        }
        has_hardware_aes() && self.is_ssh_program() && !self.options_contain_cipher_flag()
    }

    /// Checks whether the configured program appears to be an SSH client.
    ///
    /// Uses case-insensitive comparison on Windows where `SSH.EXE` or
    /// `Ssh.exe` are common depending on how the path is resolved.
    fn is_ssh_program(&self) -> bool {
        let program = self.program.to_string_lossy();
        // Handle both forward slash (Unix) and backslash (Windows) path separators.
        let basename = program.rsplit(['/', '\\']).next().unwrap_or(&program);
        if cfg!(windows) {
            let lower = basename.to_ascii_lowercase();
            lower == "ssh" || lower == "ssh.exe"
        } else {
            basename == "ssh" || basename == "ssh.exe"
        }
    }

    /// Checks whether any existing option already specifies the `-c` cipher flag.
    ///
    /// Detects three forms:
    /// - Split: `-c` as a standalone element followed by the cipher name in
    ///   the next element.
    /// - Combined: `-caes128-ctr` where the cipher name is concatenated
    ///   directly after `-c` without whitespace. SSH accepts this form.
    /// - Compound: `-c aes128-ctr` as a single unsplit string (e.g., from a
    ///   `push_option` call that did not split on whitespace).
    fn options_contain_cipher_flag(&self) -> bool {
        self.options.iter().any(|opt| {
            let s = opt.to_string_lossy();
            s == "-c" || (s.starts_with("-c") && s.len() > 2)
        })
    }

    /// Determines whether SSH keepalive options should be injected.
    ///
    /// Returns `true` only when keepalive is enabled, the program looks like
    /// an SSH client, and no existing option already specifies
    /// `ServerAliveInterval` or `ServerAliveCountMax`.
    fn should_inject_keepalive(&self) -> bool {
        self.keepalive && self.is_ssh_program() && !self.options_contain_keepalive()
    }

    /// Checks whether any existing option already specifies SSH keepalive settings.
    fn options_contain_keepalive(&self) -> bool {
        self.options.iter().any(|opt| {
            let s = opt.to_string_lossy();
            let upper = s.to_ascii_uppercase();
            upper.contains("SERVERALIVEINTERVAL") || upper.contains("SERVERALIVECOUNTMAX")
        })
    }

    /// Determines whether the SSH connect timeout option should be injected.
    ///
    /// Returns `true` only when the program looks like an SSH client and no
    /// existing option already specifies `ConnectTimeout`.
    fn should_inject_connect_timeout(&self) -> bool {
        self.is_ssh_program() && !self.options_contain_connect_timeout()
    }

    /// Checks whether any existing option already specifies `ConnectTimeout`.
    fn options_contain_connect_timeout(&self) -> bool {
        self.options.iter().any(|opt| {
            let s = opt.to_string_lossy();
            s.to_ascii_uppercase().contains("CONNECTTIMEOUT")
        })
    }

    /// Returns the connect timeout as whole seconds (rounded up), or `None`
    /// if no timeout is configured.
    fn connect_timeout_seconds(&self) -> Option<u64> {
        self.connect_timeout
            .map(|d| d.as_secs() + if d.subsec_nanos() > 0 { 1 } else { 0 })
    }

    #[cfg(test)]
    pub(crate) fn command_parts_for_testing(&self) -> (OsString, Vec<OsString>) {
        self.command_parts()
    }
}

/// Returns `true` when the CPU has hardware AES acceleration.
///
/// Hardware requirements by architecture:
///
/// - **x86 / x86_64** - requires AES-NI (Intel Westmere 2010+ or AMD
///   Bulldozer 2011+). Detected via `is_x86_feature_detected!("aes")`.
/// - **aarch64** - requires the ARMv8 Cryptography Extensions (AES
///   instructions). Detected via `is_aarch64_feature_detected!("aes")`.
///   Present on Apple M-series, AWS Graviton, and most ARMv8.1+ SoCs.
/// - **Other architectures** - always returns `false`; AES-GCM cipher
///   injection is never applied.
///
/// The result is cached in a `OnceLock` to avoid repeated feature detection
/// syscalls on platforms that probe `/proc/cpuinfo` or issue `mrs`
/// instructions.
pub(super) fn has_hardware_aes() -> bool {
    static HAS_AES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS_AES.get_or_init(|| {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::arch::is_x86_feature_detected!("aes")
        }
        #[cfg(target_arch = "aarch64")]
        {
            std::arch::is_aarch64_feature_detected!("aes")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            false
        }
    })
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
