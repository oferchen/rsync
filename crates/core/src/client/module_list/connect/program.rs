//! Connect program support for daemon connections via `RSYNC_CONNECT_PROG`.
//!
//! This module provides [`ConnectProgramConfig`] for executing custom connection
//! programs, mirroring upstream rsync's `RSYNC_CONNECT_PROG` functionality.
//!
//! # Security Model
//!
//! The `%H` (host) and `%P` (port) substitutions are performed without shell
//! escaping, matching upstream rsync behavior. This is safe because:
//!
//! - `RSYNC_CONNECT_PROG` is set by the administrator, not end users
//! - The template controls whether `%H`/`%P` are used at all
//! - Hostnames with shell metacharacters are extremely rare
//!
//! If untrusted input could reach the host parameter, callers should validate
//! the hostname format before invoking connection programs.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::super::DaemonAddress;
use crate::client::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, daemon_error};

pub(crate) fn connect_via_program(
    addr: &DaemonAddress,
    program: &ConnectProgramConfig,
) -> Result<super::DaemonStream, ClientError> {
    let command = program
        .format_command(addr.host(), addr.port())
        .map_err(|error| daemon_error(error, FEATURE_UNAVAILABLE_EXIT_CODE))?;

    let shell = program
        .shell()
        .cloned()
        .unwrap_or_else(|| OsString::from("sh"));

    let mut builder = Command::new(&shell);
    builder.arg("-c").arg(&command);
    builder.stdin(Stdio::piped());
    builder.stdout(Stdio::piped());
    builder.stderr(Stdio::inherit());
    builder.env("RSYNC_PORT", addr.port().to_string());

    let mut child = builder.spawn().map_err(|error| {
        daemon_error(
            format!(
                "failed to spawn RSYNC_CONNECT_PROG using shell '{}': {error}",
                Path::new(&shell).display()
            ),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a writable stdin",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a readable stdout",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    Ok(super::DaemonStream::program(ConnectProgramStream::new(
        child, stdin, stdout,
    )))
}

pub(crate) fn load_daemon_connect_program(
    override_template: Option<&OsStr>,
) -> Result<Option<ConnectProgramConfig>, ClientError> {
    if let Some(template) = override_template {
        if template.is_empty() {
            return Err(connect_program_configuration_error(
                "the --connect-program option requires a non-empty command",
            ));
        }

        let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());
        return ConnectProgramConfig::new(OsString::from(template), shell)
            .map(Some)
            .map_err(connect_program_configuration_error);
    }

    let Some(template) = env::var_os("RSYNC_CONNECT_PROG") else {
        return Ok(None);
    };

    if template.is_empty() {
        return Err(connect_program_configuration_error(
            "RSYNC_CONNECT_PROG must not be empty",
        ));
    }

    let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());

    ConnectProgramConfig::new(template, shell)
        .map(Some)
        .map_err(connect_program_configuration_error)
}

#[derive(Debug)]
pub(crate) struct ConnectProgramConfig {
    template: OsString,
    shell: Option<OsString>,
}

impl ConnectProgramConfig {
    pub(crate) fn new(template: OsString, shell: Option<OsString>) -> Result<Self, String> {
        if template.is_empty() {
            return Err("RSYNC_CONNECT_PROG must not be empty".to_owned());
        }

        if shell.as_ref().is_some_and(|value| value.is_empty()) {
            return Err("RSYNC_SHELL must not be empty".to_owned());
        }

        Ok(Self { template, shell })
    }

    pub(crate) const fn shell(&self) -> Option<&OsString> {
        self.shell.as_ref()
    }

    pub(crate) fn format_command(&self, host: &str, port: u16) -> Result<OsString, String> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let template = self.template.as_os_str().as_bytes();
            let mut rendered = Vec::with_capacity(template.len() + host.len());
            let mut iter = template.iter().copied();
            let host_bytes = host.as_bytes();
            let port_string = port.to_string();
            let port_bytes = port_string.as_bytes();

            while let Some(byte) = iter.next() {
                if byte == b'%' {
                    match iter.next() {
                        Some(b'%') => rendered.push(b'%'),
                        Some(b'H') => rendered.extend_from_slice(host_bytes),
                        Some(b'P') => rendered.extend_from_slice(port_bytes),
                        Some(other) => {
                            rendered.push(b'%');
                            rendered.push(other);
                        }
                        None => rendered.push(b'%'),
                    }
                } else {
                    rendered.push(byte);
                }
            }

            Ok(OsString::from_vec(rendered))
        }

        #[cfg(not(unix))]
        {
            let template = self.template.as_os_str().to_string_lossy();
            let mut rendered = String::with_capacity(template.len() + host.len());
            let mut chars = template.chars();
            let port_string = port.to_string();

            while let Some(ch) = chars.next() {
                if ch == '%' {
                    match chars.next() {
                        Some('%') => rendered.push('%'),
                        Some('H') => rendered.push_str(host),
                        Some('P') => rendered.push_str(&port_string),
                        Some(other) => {
                            rendered.push('%');
                            rendered.push(other);
                        }
                        None => rendered.push('%'),
                    }
                } else {
                    rendered.push(ch);
                }
            }

            Ok(OsString::from(rendered))
        }
    }
}

pub(crate) struct ConnectProgramStream {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ConnectProgramStream {
    const fn new(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child,
            stdin,
            stdout,
        }
    }
}

impl Read for ConnectProgramStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for ConnectProgramStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl Drop for ConnectProgramStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect_program_configuration_error(text: impl Into<String>) -> ClientError {
    let message = crate::rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, "{}", text.into())
        .with_role(crate::message::Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== ConnectProgramConfig::new tests ====================

    #[test]
    fn connect_program_config_new_valid() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None);
        assert!(config.is_ok());
    }

    #[test]
    fn connect_program_config_new_with_shell() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("/bin/bash".into()));
        assert!(config.is_ok());
        assert!(config.unwrap().shell().is_some());
    }

    #[test]
    fn connect_program_config_new_empty_template_error() {
        let config = ConnectProgramConfig::new("".into(), None);
        assert!(config.is_err());
        assert!(config.unwrap_err().contains("empty"));
    }

    #[test]
    fn connect_program_config_new_empty_shell_error() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("".into()));
        assert!(config.is_err());
        assert!(config.unwrap_err().contains("RSYNC_SHELL"));
    }

    // ==================== ConnectProgramConfig::shell tests ====================

    #[test]
    fn connect_program_config_shell_none() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        assert!(config.shell().is_none());
    }

    #[test]
    fn connect_program_config_shell_some() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), Some("/bin/zsh".into())).unwrap();
        assert_eq!(config.shell().unwrap(), "/bin/zsh");
    }

    // ==================== ConnectProgramConfig::format_command tests ====================

    #[test]
    fn format_command_substitutes_host() {
        let config = ConnectProgramConfig::new("connect to %H".into(), None).unwrap();
        let result = config.format_command("example.com", 873).unwrap();
        assert_eq!(result, "connect to example.com");
    }

    #[test]
    fn format_command_substitutes_port() {
        let config = ConnectProgramConfig::new("port %P".into(), None).unwrap();
        let result = config.format_command("host", 8080).unwrap();
        assert_eq!(result, "port 8080");
    }

    #[test]
    fn format_command_substitutes_both() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("server.local", 22).unwrap();
        assert_eq!(result, "nc server.local 22");
    }

    #[test]
    fn format_command_escapes_percent() {
        let config = ConnectProgramConfig::new("echo 100%%".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "echo 100%");
    }

    #[test]
    fn format_command_preserves_unknown_specifiers() {
        let config = ConnectProgramConfig::new("echo %X".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "echo %X");
    }

    #[test]
    fn format_command_trailing_percent() {
        let config = ConnectProgramConfig::new("test%".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "test%");
    }

    #[test]
    fn format_command_multiple_substitutions() {
        let config = ConnectProgramConfig::new("%H:%P and %H again".into(), None).unwrap();
        let result = config.format_command("myhost", 9999).unwrap();
        assert_eq!(result, "myhost:9999 and myhost again");
    }

    #[test]
    fn format_command_no_substitutions() {
        let config = ConnectProgramConfig::new("static command".into(), None).unwrap();
        let result = config.format_command("host", 873).unwrap();
        assert_eq!(result, "static command");
    }

    #[test]
    fn format_command_adjacent_specifiers() {
        let config = ConnectProgramConfig::new("%H%P".into(), None).unwrap();
        let result = config.format_command("test", 123).unwrap();
        assert_eq!(result, "test123");
    }

    #[test]
    fn format_command_complex_template() {
        let config =
            ConnectProgramConfig::new("ssh -p %P %H -o 'Port=%P' %% done".into(), None).unwrap();
        let result = config.format_command("server", 2222).unwrap();
        assert_eq!(result, "ssh -p 2222 server -o 'Port=2222' % done");
    }

    #[test]
    fn format_command_ipv6_host() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("::1", 873).unwrap();
        assert_eq!(result, "nc ::1 873");
    }

    #[test]
    fn format_command_empty_host() {
        let config = ConnectProgramConfig::new("nc '%H' %P".into(), None).unwrap();
        let result = config.format_command("", 873).unwrap();
        assert_eq!(result, "nc '' 873");
    }

    #[test]
    fn format_command_port_zero() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("host", 0).unwrap();
        assert_eq!(result, "nc host 0");
    }

    #[test]
    fn format_command_port_max() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("host", 65535).unwrap();
        assert_eq!(result, "nc host 65535");
    }

    #[test]
    fn format_command_special_chars_in_host() {
        let config = ConnectProgramConfig::new("nc %H %P".into(), None).unwrap();
        let result = config.format_command("user@host.example.com", 22).unwrap();
        assert_eq!(result, "nc user@host.example.com 22");
    }
}
