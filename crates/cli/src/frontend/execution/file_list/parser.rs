//! Operand parsing and classification.
//!
//! Determines whether CLI operands reference local or remote paths,
//! and resolves `--files-from` values into their source type.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Resolves a `--files-from` CLI value into a [`FilesFromSource`].
///
/// The resolution mirrors upstream rsync's `options.c:2447-2490`:
/// - `"-"` means stdin
/// - `:path` (colon prefix) means a remote file opened by the server
/// - Otherwise, a local file read by the client
///
/// Only the last `--files-from` argument takes effect, matching upstream
/// behaviour where later options override earlier ones.
///
/// # Upstream Reference
///
/// - `options.c:2458` - `check_for_hostspec()` detects `:path` prefix
/// - `options.c:2466-2469` - `:-` (remote stdin) is rejected
pub(crate) fn resolve_files_from_source(files_from: &[OsString]) -> core::client::FilesFromSource {
    use core::client::FilesFromSource;

    let last = match files_from.last() {
        Some(v) => v,
        None => return FilesFromSource::None,
    };

    let text = last.to_string_lossy();

    if text == "-" {
        return FilesFromSource::Stdin;
    }

    // Detect remote file: colon prefix `:path` (upstream check_for_hostspec).
    if let Some(remote_path) = text.strip_prefix(':') {
        return FilesFromSource::RemoteFile(remote_path.to_owned());
    }

    FilesFromSource::LocalFile(PathBuf::from(last))
}

/// Determines whether the transfer involves any remote operands.
///
/// Returns `true` if any element in `remainder` (the CLI operands) or
/// `file_list` (entries from `--files-from`) appears to be a remote path.
#[cfg(test)]
pub(crate) fn transfer_requires_remote(
    remainder: &[OsString],
    file_list_operands: &[OsString],
) -> bool {
    remainder
        .iter()
        .chain(file_list_operands.iter())
        .any(|operand| operand_is_remote(operand.as_os_str()))
}

#[cfg(windows)]
fn operand_has_windows_prefix(path: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    const COLON: u16 = b':' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    fn is_ascii_alpha(unit: u16) -> bool {
        (unit >= b'a' as u16 && unit <= b'z' as u16) || (unit >= b'A' as u16 && unit <= b'Z' as u16)
    }

    fn is_separator(unit: u16) -> bool {
        unit == SLASH || unit == BACKSLASH
    }

    let units: Vec<u16> = path.encode_wide().collect();
    if units.is_empty() {
        return false;
    }

    if units.len() >= 4
        && is_separator(units[0])
        && is_separator(units[1])
        && (units[2] == QUESTION || units[2] == DOT)
        && is_separator(units[3])
    {
        return true;
    }

    if units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]) {
        return true;
    }

    if units.len() >= 2 && is_ascii_alpha(units[0]) && units[1] == COLON {
        return true;
    }

    false
}

pub(crate) fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") || text.starts_with("ssh://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if operand_has_windows_prefix(path) {
            return false;
        }

        let after = &text[colon_index + 1..];
        if after.starts_with(':') {
            return true;
        }

        #[cfg(windows)]
        {
            use std::path::{Component, Path};

            if Path::new(path)
                .components()
                .next()
                .is_some_and(|component| matches!(component, Component::Prefix(_)))
            {
                return false;
            }
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        if colon_index == 1 && before.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return false;
        }

        return true;
    }

    false
}
