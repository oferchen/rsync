//! Password and authentication helpers for the CLI front-end.
//!
//! This module centralises password loading logic so the sprawling argument
//! parser in `lib.rs` can delegate to cohesive helpers. The functions here keep
//! responsibility focused on reading passwords from standard input or from
//! filesystem paths while enforcing upstream rsync's permission checks.
//! Tests operate through the exported helpers rather than touching the
//! implementation details directly, which keeps the core file smaller and easier
//! to audit.

use rsync_core::{
    message::{Message, Role},
    rsync_error,
};
use std::fs;
use std::io::{self, Read};
use std::path::Path;

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
    let metadata = fs::metadata(path).map_err(|error| {
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

    let mut bytes = fs::read(path).map_err(|error| {
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
}
