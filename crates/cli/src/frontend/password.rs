//! Password and authentication helpers for the CLI front-end.
//!
//! This module centralises password loading logic so the sprawling argument
//! parser in `lib.rs` can delegate to cohesive helpers. The functions here keep
//! responsibility focused on reading passwords from standard input, from
//! filesystem paths, or from external commands while enforcing upstream rsync's
//! permission checks.
//! Tests operate through the exported helpers rather than touching the
//! implementation details directly, which keeps the core file smaller and easier
//! to audit.

use core::{
    message::{Message, Role},
    rsync_error,
};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

#[cfg(test)]
use std::cell::RefCell;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
thread_local! {
    static PASSWORD_STDIN_INPUT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Loads an optional password file.
///
/// When `path` is `None` the function returns `Ok(None)` immediately, mirroring
/// upstream rsync's behaviour of treating the absence of a password override as
/// "no password provided". Any provided path is routed through
/// [`load_password_file`] so the standard permission checks apply.
pub(crate) fn load_optional_password(path: Option<&Path>) -> Result<Option<Vec<u8>>, Message> {
    match path {
        Some(path) => load_password_file(path).map(Some),
        None => Ok(None),
    }
}

/// Resolves the daemon password from the available sources in precedence order.
///
/// The resolution chain mirrors upstream rsync's auth_client() with the addition
/// of `--password-command`:
///
/// 1. `--password-command=COMMAND` - run COMMAND via the system shell, read stdout
/// 2. `--password-file=FILE` - read password from a file (or `-` for stdin)
///
/// Returns `Ok(None)` when neither source is specified (the caller falls through
/// to `RSYNC_PASSWORD` env var at the core layer).
pub(crate) fn resolve_password(
    password_command: Option<&OsStr>,
    password_file: Option<&Path>,
) -> Result<Option<Vec<u8>>, Message> {
    if let Some(command) = password_command {
        return load_password_command(command).map(Some);
    }
    load_optional_password(password_file)
}

/// Runs an external command via the system shell and reads the password from
/// its standard output.
///
/// The command string is passed to the platform's shell interpreter:
/// - Unix: `/bin/sh -c <command>`
/// - Windows: `cmd.exe /C <command>`
///
/// Only the first line of output is used. Trailing newlines and carriage returns
/// are stripped, matching `--password-file` semantics. The command must produce
/// at least one byte of output and must exit with status 0.
///
/// # Security model
///
/// The command string is user-provided and intentionally executed as-is. This
/// enables integration with secret managers (e.g., `pass show rsync/server`,
/// `vault read -field=password secret/rsync`). The caller is responsible for
/// the safety of the command they provide.
pub(crate) fn load_password_command(command: &OsStr) -> Result<Vec<u8>, Message> {
    let command_str = command.to_string_lossy();

    if command_str.is_empty() {
        return Err(
            rsync_error!(1, "--password-command requires a non-empty command string")
                .with_role(Role::Client),
        );
    }

    let output = build_shell_command(command).output().map_err(|error| {
        rsync_error!(
            1,
            format!(
                "failed to execute password command '{}': {}",
                command_str, error
            )
        )
        .with_role(Role::Client)
    })?;

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_owned());
        return Err(rsync_error!(
            1,
            format!(
                "password command '{}' failed with exit code {}",
                command_str, code
            )
        )
        .with_role(Role::Client));
    }

    let mut bytes = first_line(&output.stdout);
    trim_trailing_newlines(&mut bytes);

    if bytes.is_empty() {
        return Err(rsync_error!(
            1,
            format!("password command '{}' produced no output", command_str)
        )
        .with_role(Role::Client));
    }

    Ok(bytes)
}

/// Builds a [`Command`] that invokes the given string through the platform shell.
fn build_shell_command(command: &OsStr) -> Command {
    #[cfg(not(windows))]
    let (shell, flag) = ("/bin/sh", "-c");
    #[cfg(windows)]
    let (shell, flag) = ("cmd.exe", "/C");

    let mut cmd = Command::new(shell);
    cmd.arg(flag).arg(command);
    cmd
}

/// Extracts the first line from raw byte output.
fn first_line(bytes: &[u8]) -> Vec<u8> {
    if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
        bytes[..pos].to_vec()
    } else {
        bytes.to_vec()
    }
}

/// Reads a password from `path` while enforcing upstream rsync's permission rules.
///
/// The function accepts either an on-disk file or `-` to read from standard
/// input. Errors are wrapped in [`Message`] so the caller can preserve the
/// workspace's branded diagnostics.
pub(crate) fn load_password_file(path: &Path) -> Result<Vec<u8>, Message> {
    if path == Path::new("-") {
        return read_password_from_stdin().map_err(|error| {
            rsync_error!(
                1,
                format!("failed to read password from standard input: {}", error)
            )
            .with_role(Role::Client)
        });
    }

    let display = path.display();
    // Open the password file before inspecting its metadata so the
    // subsequent permission checks and read operations run against the same
    // handle. This mirrors upstream rsync's approach and avoids time-of-check
    // to time-of-use races where an attacker swaps the path after the
    // metadata check but before the read.
    let mut file = File::open(path).map_err(|error| {
        rsync_error!(
            1,
            format!("failed to access password file '{}': {}", display, error)
        )
        .with_role(Role::Client)
    })?;
    let metadata = file.metadata().map_err(|error| {
        rsync_error!(
            1,
            format!("failed to access password file '{}': {}", display, error)
        )
        .with_role(Role::Client)
    })?;

    if !metadata.is_file() {
        return Err(rsync_error!(
            1,
            format!("password file '{}' must be a regular file", display)
        )
        .with_role(Role::Client));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(
                rsync_error!(
                    1,
                    format!(
                        "password file '{}' must not be accessible to group or others (expected permissions 0600)",
                        display
                    )
                )
                .with_role(Role::Client),
            );
        }
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        rsync_error!(
            1,
            format!("failed to read password file '{}': {}", display, error)
        )
        .with_role(Role::Client)
    })?;

    trim_trailing_newlines(&mut bytes);
    Ok(bytes)
}

/// Reads a password from the process' standard input.
///
/// Tests can override the captured bytes via
/// [`set_password_stdin_input`] so the helper remains deterministic.
pub(crate) fn read_password_from_stdin() -> io::Result<Vec<u8>> {
    #[cfg(test)]
    if let Some(bytes) = take_password_stdin_input() {
        let mut cursor = std::io::Cursor::new(bytes);
        return read_password_from_reader(&mut cursor);
    }

    let mut stdin = io::stdin().lock();
    read_password_from_reader(&mut stdin)
}

/// Reads a password from an arbitrary reader, trimming trailing newlines.
pub(crate) fn read_password_from_reader<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    trim_trailing_newlines(&mut bytes);
    Ok(bytes)
}

fn trim_trailing_newlines(bytes: &mut Vec<u8>) {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
}

#[cfg(test)]
fn take_password_stdin_input() -> Option<Vec<u8>> {
    PASSWORD_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

/// Installs bytes that will be consumed by [`read_password_from_stdin`] in tests.
#[cfg(test)]
pub(crate) fn set_password_stdin_input(data: Vec<u8>) {
    PASSWORD_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Write;

    use tempfile::NamedTempFile;
    #[cfg(unix)]
    use tempfile::tempdir;

    #[test]
    fn trims_trailing_newlines() {
        let mut bytes = b"secret\n\r".to_vec();
        trim_trailing_newlines(&mut bytes);
        assert_eq!(bytes, b"secret");
    }

    #[test]
    fn load_password_from_stdin_uses_override() {
        set_password_stdin_input(b"stdin-secret\n".to_vec());
        let password = read_password_from_stdin().expect("stdin override");
        assert_eq!(password, b"stdin-secret");
    }

    #[test]
    fn load_optional_password_absent_returns_none() {
        assert_eq!(load_optional_password(None).expect("optional load"), None);
    }

    #[test]
    fn load_optional_password_reads_file_contents() {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(b"from-file\n").expect("write secret");
        let path = file.into_temp_path();

        let loaded = load_optional_password(Some(path.as_ref())).expect("load password");

        assert_eq!(loaded, Some(b"from-file".to_vec()));
    }

    #[test]
    fn load_password_file_reads_from_standard_input() {
        set_password_stdin_input(b"stdin-file\n".to_vec());
        let password = load_password_file(Path::new("-")).expect("stdin password");

        assert_eq!(password, b"stdin-file".to_vec());
    }

    #[test]
    #[cfg(unix)]
    fn load_password_file_rejects_non_file_paths() {
        // On Unix, File::open() on a directory succeeds but we check is_file()
        // and return a "must be a regular file" error. On Windows, File::open()
        // on a directory fails with "Access denied" before we reach the check.
        let dir = tempdir().expect("temporary directory");
        let error = load_password_file(dir.path()).expect_err("directories rejected");

        assert_eq!(error.code(), Some(1));
        assert_eq!(error.role(), Some(Role::Client));
        assert!(
            error.text().contains("must be a regular file"),
            "unexpected diagnostic: {}",
            error.text()
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_password_file_rejects_group_or_world_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(b"perms").expect("write secret");

        let permissions = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(file.path(), permissions).expect("set permissions");

        let error = load_password_file(file.path()).expect_err("insecure permissions");

        assert_eq!(error.code(), Some(1));
        assert_eq!(error.role(), Some(Role::Client));
        assert!(
            error
                .text()
                .contains("must not be accessible to group or others"),
            "unexpected diagnostic: {}",
            error.text()
        );
    }

    #[test]
    fn first_line_extracts_before_newline() {
        assert_eq!(first_line(b"hello\nworld"), b"hello");
        assert_eq!(first_line(b"only-line"), b"only-line");
        assert_eq!(first_line(b""), b"");
        assert_eq!(first_line(b"\n"), b"");
        assert_eq!(first_line(b"line1\nline2\nline3"), b"line1");
    }

    #[test]
    fn load_password_command_reads_echo_output() {
        let cmd = OsString::from("echo cmd-secret");

        let password = load_password_command(&cmd).expect("echo command");
        assert_eq!(password, b"cmd-secret");
    }

    #[test]
    fn load_password_command_strips_trailing_newlines() {
        let cmd = if cfg!(windows) {
            OsString::from("echo cmd-stripped")
        } else {
            OsString::from("printf 'cmd-stripped\\n\\r\\n'")
        };

        let password = load_password_command(&cmd).expect("newline stripping");
        assert_eq!(password, b"cmd-stripped");
    }

    #[test]
    fn load_password_command_takes_only_first_line() {
        #[cfg(unix)]
        {
            let cmd = OsString::from("printf 'first-line\\nsecond-line\\n'");
            let password = load_password_command(&cmd).expect("first line only");
            assert_eq!(password, b"first-line");
        }
    }

    #[test]
    fn load_password_command_rejects_empty_command() {
        let error = load_password_command(OsStr::new("")).expect_err("empty command");

        assert_eq!(error.code(), Some(1));
        assert_eq!(error.role(), Some(Role::Client));
        assert!(
            error.text().contains("non-empty command string"),
            "unexpected diagnostic: {}",
            error.text()
        );
    }

    #[test]
    fn load_password_command_rejects_failing_command() {
        let cmd = OsString::from("exit 42");
        let error = load_password_command(&cmd).expect_err("non-zero exit");

        assert_eq!(error.code(), Some(1));
        assert_eq!(error.role(), Some(Role::Client));
        assert!(
            error.text().contains("failed with exit code 42"),
            "unexpected diagnostic: {}",
            error.text()
        );
    }

    #[test]
    fn load_password_command_rejects_empty_output() {
        let cmd = OsString::from("true");
        let error = load_password_command(&cmd).expect_err("empty output");

        assert_eq!(error.code(), Some(1));
        assert_eq!(error.role(), Some(Role::Client));
        assert!(
            error.text().contains("produced no output"),
            "unexpected diagnostic: {}",
            error.text()
        );
    }

    #[test]
    fn resolve_password_prefers_command_over_file() {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(b"file-secret\n").expect("write secret");
        let path = file.into_temp_path();

        let cmd = OsString::from("echo cmd-secret");
        let password = resolve_password(Some(&cmd), Some(path.as_ref())).expect("resolve");

        assert_eq!(password, Some(b"cmd-secret".to_vec()));
    }

    #[test]
    fn resolve_password_falls_back_to_file() {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(b"file-secret\n").expect("write secret");
        let path = file.into_temp_path();

        let password = resolve_password(None, Some(path.as_ref())).expect("resolve");
        assert_eq!(password, Some(b"file-secret".to_vec()));
    }

    #[test]
    fn resolve_password_returns_none_when_no_source() {
        let password = resolve_password(None, None).expect("resolve");
        assert_eq!(password, None);
    }
}
