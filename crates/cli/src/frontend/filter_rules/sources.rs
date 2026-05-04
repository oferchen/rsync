use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use core::client::{FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

pub(crate) fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    for path in files {
        let patterns = load_filter_file_patterns(Path::new(path.as_os_str()))?;
        destination.extend(patterns.into_iter().map(|pattern| match kind {
            FilterRuleKind::Include => FilterRuleSpec::include(pattern),
            FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
            FilterRuleKind::Clear => FilterRuleSpec::clear(),
            FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
            FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
            FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
        }));
    }
    Ok(())
}

pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    if path == Path::new("-") {
        return read_filter_patterns_from_standard_input();
    }

    let path_display = path.display().to_string();
    let file = File::open(path).map_err(|error| {
        let text = format!("failed to read filter file '{path_display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })?;

    let mut reader = BufReader::new(file);
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter file '{path_display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_merge_file(path: &Path) -> Result<String, Message> {
    let display = path.display();
    fs::read_to_string(path).map_err(|error| {
        let text = format!("failed to read filter file '{display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_merge_from_standard_input() -> Result<String, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        return String::from_utf8(data).map_err(|error| {
            let text = format!("failed to read filter patterns from standard input: {error}");
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).map_err(|error| {
        let text = format!("failed to read filter patterns from standard input: {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })?;
    Ok(buffer)
}

pub(crate) fn read_filter_patterns_from_standard_input() -> Result<Vec<String>, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        let mut cursor = io::Cursor::new(data);
        return read_filter_patterns(&mut cursor).map_err(|error| {
            let text = format!("failed to read filter patterns from standard input: {error}");
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter patterns from standard input: {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_filter_patterns<R: BufRead>(reader: &mut R) -> io::Result<Vec<String>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();

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

        let line = String::from_utf8_lossy(&buffer);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        patterns.push(line.into_owned());
    }

    Ok(patterns)
}

#[cfg(test)]
thread_local! {
    static FILTER_STDIN_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(super) fn take_filter_stdin_input() -> Option<Vec<u8>> {
    FILTER_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    FILTER_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[test]
    fn read_filter_patterns_parses_simple_lines() {
        let input = b"pattern1\npattern2\npattern3\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_skips_empty_lines() {
        let input = b"pattern1\n\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_hash_comments() {
        let input = b"pattern1\n# this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_semicolon_comments() {
        let input = b"pattern1\n; this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_crlf_line_endings() {
        let input = b"pattern1\r\npattern2\r\npattern3\r\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_handles_no_trailing_newline() {
        let input = b"pattern1\npattern2";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_handles_only_comments() {
        let input = b"# comment 1\n; comment 2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_handles_whitespace_only_lines() {
        let input = b"pattern1\n   \n\t\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_preserves_leading_whitespace() {
        let input = b"  pattern_with_leading_space\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader).expect("read");
        assert_eq!(result, vec!["  pattern_with_leading_space"]);
    }

    #[test]
    fn load_filter_file_patterns_reads_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("filter.txt");
        std::fs::write(&path, "pattern1\npattern2\n").expect("write");
        let result = load_filter_file_patterns(&path).expect("load");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn load_filter_file_patterns_fails_for_missing_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");
        let result = load_filter_file_patterns(&path);
        assert!(result.is_err());
    }

    #[test]
    fn read_merge_file_reads_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("merge.txt");
        std::fs::write(&path, "content here").expect("write");
        let result = read_merge_file(&path).expect("read");
        assert_eq!(result, "content here");
    }

    #[test]
    fn read_merge_file_fails_for_missing_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");
        let result = read_merge_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn read_filter_patterns_from_stdin_uses_test_input() {
        set_filter_stdin_input(b"stdin_pattern1\nstdin_pattern2\n".to_vec());
        let result = read_filter_patterns_from_standard_input().expect("read");
        assert_eq!(result, vec!["stdin_pattern1", "stdin_pattern2"]);
    }

    #[test]
    fn read_merge_from_stdin_uses_test_input() {
        set_filter_stdin_input(b"stdin content here".to_vec());
        let result = read_merge_from_standard_input().expect("read");
        assert_eq!(result, "stdin content here");
    }

    #[test]
    fn load_filter_file_patterns_handles_dash_path() {
        set_filter_stdin_input(b"stdin_pattern\n".to_vec());
        let result = load_filter_file_patterns(Path::new("-")).expect("load");
        assert_eq!(result, vec!["stdin_pattern"]);
    }

    #[test]
    fn append_filter_rules_from_files_adds_include_rules() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("include.txt");
        std::fs::write(&path, "*.rs\n*.toml\n").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Include,
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].kind(), FilterRuleKind::Include);
        assert_eq!(rules[0].pattern(), "*.rs");
        assert_eq!(rules[1].kind(), FilterRuleKind::Include);
        assert_eq!(rules[1].pattern(), "*.toml");
    }

    #[test]
    fn append_filter_rules_from_files_adds_exclude_rules() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("exclude.txt");
        std::fs::write(&path, "*.bak\n").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Exclude,
        )
        .expect("append");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind(), FilterRuleKind::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn append_filter_rules_from_files_rejects_dir_merge() {
        let mut rules = Vec::new();
        let result = append_filter_rules_from_files(&mut rules, &[], FilterRuleKind::DirMerge);
        assert!(result.is_err());
    }

    #[test]
    fn append_filter_rules_from_files_handles_multiple_files() {
        let temp = tempdir().expect("tempdir");
        let path1 = temp.path().join("file1.txt");
        let path2 = temp.path().join("file2.txt");
        std::fs::write(&path1, "pattern1\n").expect("write");
        std::fs::write(&path2, "pattern2\n").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path1), OsString::from(path2)],
            FilterRuleKind::Include,
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
    }
}
