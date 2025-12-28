use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use core::{
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

pub(crate) fn resolve_file_list_entries(
    entries: &mut [OsString],
    explicit_operands: &[OsString],
    relative_enabled: bool,
) {
    if entries.is_empty() || explicit_operands.len() <= 1 || relative_enabled {
        return;
    }

    let base_sources = &explicit_operands[..explicit_operands.len() - 1];
    if base_sources.len() != 1 {
        return;
    }

    let base = &base_sources[0];
    if operand_is_remote(base.as_os_str()) {
        return;
    }

    let base_path = Path::new(base);
    for entry in entries.iter_mut() {
        if entry.is_empty() {
            continue;
        }

        if operand_is_remote(entry.as_os_str()) {
            continue;
        }

        let entry_path = Path::new(entry);
        if entry_path.is_absolute() {
            continue;
        }

        let mut combined = base_path.to_path_buf();
        combined.push(entry_path);
        *entry = combined.into_os_string();
    }
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

/// Deprecated: Kept for reference, will be removed once native SSH is fully validated
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ==================== operand_is_remote tests ====================

    #[test]
    fn operand_is_remote_rsync_url() {
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
        assert!(operand_is_remote(OsStr::new("rsync://localhost/path")));
        assert!(operand_is_remote(OsStr::new("rsync://user@host/mod")));
    }

    #[test]
    fn operand_is_remote_double_colon() {
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("user@host::module/path")));
        assert!(operand_is_remote(OsStr::new("server.example.com::backup")));
    }

    #[test]
    fn operand_is_remote_ssh_style() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("user@host:path/to/file")));
        assert!(operand_is_remote(OsStr::new("server:/etc/config")));
    }

    #[test]
    fn operand_is_remote_local_paths() {
        assert!(!operand_is_remote(OsStr::new("/home/user/file.txt")));
        assert!(!operand_is_remote(OsStr::new("relative/path")));
        assert!(!operand_is_remote(OsStr::new("./local")));
        assert!(!operand_is_remote(OsStr::new("../parent")));
    }

    #[test]
    fn operand_is_remote_local_path_with_slash_before_colon() {
        // Paths with a slash before the colon are local
        assert!(!operand_is_remote(OsStr::new("/path/with:colon")));
        assert!(!operand_is_remote(OsStr::new("some/path:with:colons")));
    }

    #[test]
    fn operand_is_remote_empty_string() {
        assert!(!operand_is_remote(OsStr::new("")));
    }

    #[test]
    fn operand_is_remote_no_colon() {
        assert!(!operand_is_remote(OsStr::new("simple-filename")));
        assert!(!operand_is_remote(OsStr::new("path/to/file")));
    }

    // ==================== read_file_list_from_reader tests ====================

    #[test]
    fn read_file_list_newline_terminated() {
        let input = b"file1.txt\nfile2.txt\nfile3.txt\n";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
        assert_eq!(entries[2], "file3.txt");
    }

    #[test]
    fn read_file_list_no_trailing_newline() {
        let input = b"file1.txt\nfile2.txt";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
    }

    #[test]
    fn read_file_list_crlf_line_endings() {
        let input = b"file1.txt\r\nfile2.txt\r\n";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
    }

    #[test]
    fn read_file_list_skips_comments() {
        let input = b"# This is a comment\nfile1.txt\n; Also a comment\nfile2.txt\n";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
    }

    #[test]
    fn read_file_list_zero_terminated() {
        let input = b"file1.txt\0file2.txt\0file3.txt\0";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, true, &mut entries).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
        assert_eq!(entries[2], "file3.txt");
    }

    #[test]
    fn read_file_list_zero_terminated_no_trailing_null() {
        let input = b"file1.txt\0file2.txt";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, true, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "file1.txt");
        assert_eq!(entries[1], "file2.txt");
    }

    #[test]
    fn read_file_list_zero_terminated_allows_hash_and_semicolon() {
        // With zero termination, # and ; are not treated as comments
        let input = b"#not-a-comment\0;also-not-a-comment\0";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, true, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "#not-a-comment");
        assert_eq!(entries[1], ";also-not-a-comment");
    }

    #[test]
    fn read_file_list_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert!(entries.is_empty());
    }

    #[test]
    fn read_file_list_empty_lines_skipped() {
        let input = b"file1.txt\n\n\nfile2.txt\n";
        let mut reader = Cursor::new(input);
        let mut entries = Vec::new();

        read_file_list_from_reader(&mut reader, false, &mut entries).unwrap();

        assert_eq!(entries.len(), 2);
    }

    // ==================== resolve_file_list_entries tests ====================

    #[test]
    fn resolve_file_list_entries_empty_entries() {
        let mut entries: Vec<OsString> = Vec::new();
        let operands = vec![OsString::from("/base"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        assert!(entries.is_empty());
    }

    #[test]
    fn resolve_file_list_entries_single_operand_no_change() {
        let mut entries = vec![OsString::from("file.txt")];
        let operands = vec![OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        // Single operand means no base path to prepend
        assert_eq!(entries[0], "file.txt");
    }

    #[test]
    fn resolve_file_list_entries_relative_enabled() {
        let mut entries = vec![OsString::from("file.txt")];
        let operands = vec![OsString::from("/base"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, true);

        // With relative paths enabled, no resolution happens
        assert_eq!(entries[0], "file.txt");
    }

    #[test]
    fn resolve_file_list_entries_prepends_base() {
        let mut entries = vec![OsString::from("subdir/file.txt")];
        let operands = vec![OsString::from("/base/path"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        assert_eq!(entries[0], OsString::from("/base/path/subdir/file.txt"));
    }

    #[test]
    fn resolve_file_list_entries_absolute_path_unchanged() {
        let mut entries = vec![OsString::from("/absolute/path.txt")];
        let operands = vec![OsString::from("/base"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        // Absolute paths are not modified
        assert_eq!(entries[0], "/absolute/path.txt");
    }

    #[test]
    fn resolve_file_list_entries_remote_base_no_change() {
        let mut entries = vec![OsString::from("file.txt")];
        let operands = vec![OsString::from("host:path"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        // Remote base path means no resolution
        assert_eq!(entries[0], "file.txt");
    }

    #[test]
    fn resolve_file_list_entries_remote_entry_no_change() {
        let mut entries = vec![OsString::from("host:remote/file.txt")];
        let operands = vec![OsString::from("/base"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        // Remote entries are not modified
        assert_eq!(entries[0], "host:remote/file.txt");
    }

    #[test]
    fn resolve_file_list_entries_multiple_sources_no_change() {
        let mut entries = vec![OsString::from("file.txt")];
        let operands = vec![
            OsString::from("/source1"),
            OsString::from("/source2"),
            OsString::from("/dest"),
        ];

        resolve_file_list_entries(&mut entries, &operands, false);

        // Multiple source operands means no single base, so no resolution
        assert_eq!(entries[0], "file.txt");
    }

    #[test]
    fn resolve_file_list_entries_empty_entry_unchanged() {
        let mut entries = vec![OsString::from(""), OsString::from("file.txt")];
        let operands = vec![OsString::from("/base"), OsString::from("/dest")];

        resolve_file_list_entries(&mut entries, &operands, false);

        assert_eq!(entries[0], "");
        assert_eq!(entries[1], OsString::from("/base/file.txt"));
    }

    // ==================== transfer_requires_remote tests ====================

    #[test]
    fn transfer_requires_remote_all_local() {
        let remainder = vec![OsString::from("/local/path"), OsString::from("/dest")];
        let file_list: Vec<OsString> = vec![];

        assert!(!transfer_requires_remote(&remainder, &file_list));
    }

    #[test]
    fn transfer_requires_remote_with_remote_operand() {
        let remainder = vec![OsString::from("host:path"), OsString::from("/dest")];
        let file_list: Vec<OsString> = vec![];

        assert!(transfer_requires_remote(&remainder, &file_list));
    }

    #[test]
    fn transfer_requires_remote_with_remote_in_file_list() {
        let remainder = vec![OsString::from("/local"), OsString::from("/dest")];
        let file_list = vec![OsString::from("host:remote/file")];

        assert!(transfer_requires_remote(&remainder, &file_list));
    }

    #[test]
    fn transfer_requires_remote_both_empty() {
        let remainder: Vec<OsString> = vec![];
        let file_list: Vec<OsString> = vec![];

        assert!(!transfer_requires_remote(&remainder, &file_list));
    }

    // ==================== push_file_list_entry tests ====================

    #[test]
    fn push_file_list_entry_empty_bytes() {
        let mut entries = Vec::new();
        push_file_list_entry(b"", &mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn push_file_list_entry_simple() {
        let mut entries = Vec::new();
        push_file_list_entry(b"filename.txt", &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "filename.txt");
    }

    #[test]
    fn push_file_list_entry_strips_trailing_cr() {
        let mut entries = Vec::new();
        push_file_list_entry(b"filename.txt\r\r\r", &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "filename.txt");
    }

    #[test]
    fn push_file_list_entry_all_cr_produces_nothing() {
        let mut entries = Vec::new();
        push_file_list_entry(b"\r\r\r", &mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn push_file_list_entry_preserves_internal_cr() {
        let mut entries = Vec::new();
        push_file_list_entry(b"file\rname.txt", &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "file\rname.txt");
    }

    #[test]
    fn push_file_list_entry_with_path() {
        let mut entries = Vec::new();
        push_file_list_entry(b"path/to/file.txt", &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "path/to/file.txt");
    }
}
