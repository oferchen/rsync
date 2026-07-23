use std::ffi::{OsStr, OsString};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use super::loader::{push_file_list_entry, read_file_list_from_reader};
use super::parser::{operand_is_remote, resolve_files_from_source, transfer_requires_remote};
use super::resolver::resolve_file_list_entries;

#[test]
fn operand_is_remote_rsync_url() {
    assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
    assert!(operand_is_remote(OsStr::new("rsync://localhost/path")));
    assert!(operand_is_remote(OsStr::new("rsync://user@host/mod")));
}

#[test]
fn operand_is_remote_ssh_url() {
    assert!(operand_is_remote(OsStr::new("ssh://host/path")));
    assert!(operand_is_remote(OsStr::new("ssh://user@host/path")));
    assert!(operand_is_remote(OsStr::new(
        "ssh://user@host:2222/path/to/file"
    )));
    assert!(operand_is_remote(OsStr::new("ssh://host/~/data")));
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
    // A slash before the colon disambiguates local paths from host:path syntax.
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
fn read_file_list_zero_terminated_strips_hash_and_semicolon_comments() {
    // upstream: flist.c:2249 sets RL_DUMP_COMMENTS independent of eol_nulls,
    // and io.c:1276 read_line() strips leading '#'/';' comment lines even with
    // NUL delimiters. Comment entries are dropped; normal entries are kept.
    let input = b"#comment\0keep.txt\0;also-comment\0other.txt\0";
    let mut reader = Cursor::new(input);
    let mut entries = Vec::new();

    read_file_list_from_reader(&mut reader, true, &mut entries).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], "keep.txt");
    assert_eq!(entries[1], "other.txt");
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

#[test]
fn resolve_file_list_entries_empty_entries() {
    let mut entries: Vec<OsString> = Vec::new();
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    assert!(entries.is_empty());
}

#[test]
fn resolve_file_list_entries_single_operand_no_change() {
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    // With one operand there is no source base to prepend.
    assert_eq!(entries[0], "file.txt");
}

#[test]
fn resolve_file_list_entries_relative_enabled() {
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, true, false);

    // upstream: --relative defers path resolution to the receiver.
    assert_eq!(entries[0], "file.txt");
}

#[test]
fn resolve_file_list_entries_prepends_base() {
    let mut entries = vec![OsString::from("subdir/file.txt")];
    let operands = vec![OsString::from("/base/path"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    // Path::join uses the platform separator (verified via Path, not literal).
    let expected = Path::new("/base/path").join("subdir/file.txt");
    assert_eq!(entries[0], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_absolute_path_unchanged() {
    let mut entries = vec![OsString::from("/absolute/path.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    assert_eq!(entries[0], "/absolute/path.txt");
}

#[test]
fn resolve_file_list_entries_remote_base_no_change() {
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("host:path"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    assert_eq!(entries[0], "file.txt");
}

#[test]
fn resolve_file_list_entries_remote_entry_no_change() {
    let mut entries = vec![OsString::from("host:remote/file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

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

    resolve_file_list_entries(&mut entries, &operands, false, false);

    // Multiple sources have no single base to resolve against.
    assert_eq!(entries[0], "file.txt");
}

#[test]
fn resolve_file_list_entries_empty_entry_unchanged() {
    let mut entries = vec![OsString::from(""), OsString::from("file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, false);

    assert_eq!(entries[0], "");
    let expected = Path::new("/base").join("file.txt");
    assert_eq!(entries[1], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_files_from_inserts_dot_marker() {
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    let expected = Path::new("/base").join(".").join("file.txt");
    assert_eq!(entries[0], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_files_from_nested_path_with_marker() {
    let mut entries = vec![OsString::from("subdir/file.txt")];
    let operands = vec![OsString::from("/base/path"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    let expected = Path::new("/base/path").join(".").join("subdir/file.txt");
    assert_eq!(entries[0], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_files_from_with_relative_still_inserts_marker() {
    // upstream: --files-from always inserts the "/./" marker, including when
    // --relative is active.
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, true, true);

    let expected = Path::new("/base").join(".").join("file.txt");
    assert_eq!(entries[0], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_files_from_absolute_entry_unchanged() {
    let mut entries = vec![OsString::from("/absolute/path.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    assert_eq!(entries[0], "/absolute/path.txt");
}

#[test]
fn resolve_file_list_entries_files_from_empty_entry_unchanged() {
    let mut entries = vec![OsString::from(""), OsString::from("file.txt")];
    let operands = vec![OsString::from("/base"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    assert_eq!(entries[0], "");
    let expected = Path::new("/base").join(".").join("file.txt");
    assert_eq!(entries[1], expected.as_os_str());
}

#[test]
fn resolve_file_list_entries_files_from_remote_base_no_change() {
    let mut entries = vec![OsString::from("file.txt")];
    let operands = vec![OsString::from("host:path"), OsString::from("/dest")];

    resolve_file_list_entries(&mut entries, &operands, false, true);

    assert_eq!(entries[0], "file.txt");
}

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

#[test]
fn resolve_files_from_empty_returns_none() {
    let files: Vec<OsString> = vec![];
    let source = resolve_files_from_source(&files);
    assert_eq!(source, core::client::FilesFromSource::None);
}

#[test]
fn resolve_files_from_stdin() {
    let files = vec![OsString::from("-")];
    let source = resolve_files_from_source(&files);
    assert_eq!(source, core::client::FilesFromSource::Stdin);
}

#[test]
fn resolve_files_from_local_file() {
    let files = vec![OsString::from("/path/to/file.txt")];
    let source = resolve_files_from_source(&files);
    assert_eq!(
        source,
        core::client::FilesFromSource::LocalFile(PathBuf::from("/path/to/file.txt"))
    );
}

#[test]
fn resolve_files_from_remote_colon_prefix() {
    let files = vec![OsString::from(":/remote/path.txt")];
    let source = resolve_files_from_source(&files);
    assert_eq!(
        source,
        core::client::FilesFromSource::RemoteFile("/remote/path.txt".to_owned())
    );
}

#[test]
fn resolve_files_from_host_colon_prefix() {
    // UTS-V3-D regression: upstream's `testsuite/files-from.test` 4th
    // invocation passes `--files-from=localhost:scratch/filelist`. The
    // parser must recognise the hostspec form (upstream
    // `options.c:3112-3138 check_for_hostspec`) and emit RemoteFile
    // with the host stripped. Without this fix the loader hit
    // `loader.rs:54` immediately with "No such file or directory".
    let files = vec![OsString::from("localhost:/remote/path.txt")];
    let source = resolve_files_from_source(&files);
    assert_eq!(
        source,
        core::client::FilesFromSource::RemoteFile("/remote/path.txt".to_owned())
    );
}

#[test]
fn resolve_files_from_windows_drive_letter_is_local() {
    // upstream check_for_hostspec rejects single-letter "host" because
    // parse_hostspec demands a real hostname. The DOS drive-letter form
    // must continue to resolve as a local path on Windows.
    let files = vec![OsString::from("C:/tmp/list.txt")];
    let source = resolve_files_from_source(&files);
    match source {
        core::client::FilesFromSource::LocalFile(p) => {
            assert_eq!(p.to_string_lossy(), "C:/tmp/list.txt");
        }
        other => panic!("expected LocalFile for drive-letter, got {other:?}"),
    }
}

#[test]
fn resolve_files_from_daemon_module_spec_is_local() {
    // `host::module` is the daemon module access form, handled by a
    // separate transport path. The files-from parser must defer to
    // LocalFile so the daemon path produces its own error.
    let files = vec![OsString::from("host::mod/list")];
    let source = resolve_files_from_source(&files);
    match source {
        core::client::FilesFromSource::LocalFile(p) => {
            assert_eq!(p.to_string_lossy(), "host::mod/list");
        }
        other => panic!("expected LocalFile for daemon spec, got {other:?}"),
    }
}

#[test]
fn resolve_files_from_remote_stdin_marker_falls_through() {
    // upstream `options.c:2466-2469` aborts on `host:-`; we route through
    // LocalFile so the downstream loader returns a clear error rather
    // than silently routing the literal `-` to the remote side.
    let files = vec![OsString::from("host:-")];
    let source = resolve_files_from_source(&files);
    match source {
        core::client::FilesFromSource::LocalFile(_) => {}
        other => panic!("expected LocalFile fall-through for host:-, got {other:?}"),
    }
}

#[test]
fn resolve_files_from_uses_last_value() {
    let files = vec![OsString::from("/first.txt"), OsString::from("/last.txt")];
    let source = resolve_files_from_source(&files);
    assert_eq!(
        source,
        core::client::FilesFromSource::LocalFile(PathBuf::from("/last.txt"))
    );
}

#[test]
fn resolve_files_from_relative_path() {
    let files = vec![OsString::from("relative/path.txt")];
    let source = resolve_files_from_source(&files);
    assert_eq!(
        source,
        core::client::FilesFromSource::LocalFile(PathBuf::from("relative/path.txt"))
    );
}

#[cfg(windows)]
mod windows_operand_detection {
    use super::*;

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
