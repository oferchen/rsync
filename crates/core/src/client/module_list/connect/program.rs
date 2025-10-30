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

pub(crate) struct ConnectProgramConfig {
    template: OsString,
    shell: Option<OsString>,
}

impl ConnectProgramConfig {
    pub(crate) fn new(template: OsString, shell: Option<OsString>) -> Result<Self, String> {
        if template.is_empty() {
            return Err("RSYNC_CONNECT_PROG must not be empty".to_string());
        }

        if shell.as_ref().is_some_and(|value| value.is_empty()) {
            return Err("RSYNC_SHELL must not be empty".to_string());
        }

        Ok(Self { template, shell })
    }

    pub(crate) fn shell(&self) -> Option<&OsString> {
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

            return Ok(OsString::from_vec(rendered));
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
    fn new(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
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
