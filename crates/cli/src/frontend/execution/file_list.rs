use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;

use rsync_core::{
    message::{Message, Role},
    rsync_error,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

/// Loads operands referenced by `--files-from` arguments.
///
/// When `zero_terminated` is `false`, the reader treats lines beginning with `#`
/// or `;` as comments, matching upstream rsync. Supplying `--from0` disables the
/// comment semantics so entries can legitimately start with those bytes.
pub(crate) fn load_file_list_operands(
    files: &[OsString],
    zero_terminated: bool,
) -> Result<Vec<OsString>, Message> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut stdin_handle: Option<io::Stdin> = None;

    for path in files {
        if path.as_os_str() == OsStr::new("-") {
            let stdin = stdin_handle.get_or_insert_with(io::stdin);
            let mut reader = stdin.lock();
            read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
                |error| {
                    rsync_error!(
                        1,
                        format!("failed to read file list from standard input: {error}")
                    )
                    .with_role(Role::Client)
                },
            )?;
            continue;
        }

        let path_buf = PathBuf::from(path);
        let display = path_buf.display().to_string();
        let file = File::open(&path_buf).map_err(|error| {
            rsync_error!(
                1,
                format!("failed to read file list '{}': {}", display, error)
            )
            .with_role(Role::Client)
        })?;
        let mut reader = BufReader::new(file);
        read_file_list_from_reader(&mut reader, zero_terminated, &mut entries).map_err(
            |error| {
                rsync_error!(
                    1,
                    format!("failed to read file list '{}': {}", display, error)
                )
                .with_role(Role::Client)
            },
        )?;
    }

    Ok(entries)
}

pub(crate) fn read_file_list_from_reader<R: BufRead>(
    reader: &mut R,
    zero_terminated: bool,
    entries: &mut Vec<OsString>,
) -> io::Result<()> {
    if zero_terminated {
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let read = reader.read_until(b'\0', &mut buffer)?;
            if read == 0 {
                break;
            }

            if buffer.last() == Some(&b'\0') {
                buffer.pop();
            }

            push_file_list_entry(&buffer, entries);
        }
        return Ok(());
    }

    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        if buffer
            .first()
            .is_some_and(|byte| matches!(byte, b'#' | b';'))
        {
            continue;
        }

        push_file_list_entry(&buffer, entries);
    }

    Ok(())
}

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

    if text.starts_with("rsync://") {
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

#[cfg(all(test, windows))]
mod windows_operand_detection {
    use super::operand_is_remote;
    use std::ffi::OsStr;

    #[test]
    fn drive_letter_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(r"c:relative\\path")));
    }

    #[test]
    fn extended_prefixes_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"\\\\?\\C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\?\\UNC\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new(r"\\\\.\\pipe\\rsync")));
    }

    #[test]
    fn unc_and_forward_slash_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new("//server/share/file.txt")));
    }

    #[test]
    fn remote_operands_remain_remote() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("user@host:path")));
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
    }
}

fn push_file_list_entry(bytes: &[u8], entries: &mut Vec<OsString>) {
    if bytes.is_empty() {
        return;
    }

    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }

    if end > 0 {
        let trimmed = &bytes[..end];

        #[cfg(unix)]
        {
            if !trimmed.is_empty() {
                entries.push(OsString::from_vec(trimmed.to_vec()));
            }
        }

        #[cfg(not(unix))]
        {
            let text = String::from_utf8_lossy(trimmed).into_owned();
            if !text.is_empty() {
                entries.push(OsString::from(text));
            }
        }
    }
}
