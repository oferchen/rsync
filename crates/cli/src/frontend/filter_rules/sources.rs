use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use core::client::{FILE_IO_EXIT_CODE, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;
use engine::local_copy::upstream_io_error;

use filters::RuleSource;

use super::directive::FilterDirective;
use super::parsing::parse_old_prefix_rule;

/// Loads filter patterns from `--include-from` / `--exclude-from` files and
/// appends the resulting rules to `destination`. Include/exclude files honor the
/// old-prefix syntax (`- `/`+ `/`!`) with each file's `!` clear scoped locally;
/// dir-merge is rejected. `eol_nulls` selects NUL-delimited records (`--from0`).
pub(crate) fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
    eol_nulls: bool,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    // upstream: options.c:1541-1543 - --exclude-from and --include-from
    // feed parse_filter_file with XFLG_OLD_PREFIXES so per-line `- pat`,
    // `+ pat`, and `!` prefixes flip the rule kind or clear the list. Kinds
    // other than Include/Exclude have no upstream OLD_PREFIXES analogue, so
    // we keep the legacy raw-pattern behavior to preserve existing semantics.
    let honor_old_prefixes = matches!(kind, FilterRuleKind::Include | FilterRuleKind::Exclude);

    // upstream: exclude.c:1393-1402 resolves FILTRULE_CLEAR_LIST via
    // pop_filter_list (exclude.c:574-590), truncating only the local section
    // of the per-file rule list. Buffer each --include-from/--exclude-from
    // file's rules locally so a `!` inside the file clears only that scope
    // and parent CLI rules survive.
    for path in files {
        let patterns = load_filter_file_patterns(
            Path::new(path.as_os_str()),
            eol_nulls,
            matches!(kind, FilterRuleKind::Include),
        )?;
        let mut local: Vec<FilterRuleSpec> = Vec::new();
        for pattern in patterns {
            if honor_old_prefixes {
                match parse_old_prefix_rule(&pattern, kind)? {
                    FilterDirective::Rule(rule) => local.push(rule),
                    FilterDirective::Clear => local.clear(),
                    // A blank line in an exclude-from/include-from file is skipped.
                    FilterDirective::Noop => {}
                    FilterDirective::Merge(_) | FilterDirective::CvsDefaults => {
                        unreachable!(
                            "parse_old_prefix_rule never emits FilterDirective::Merge or CvsDefaults"
                        )
                    }
                }
            } else {
                local.push(match kind {
                    FilterRuleKind::Include => FilterRuleSpec::include(pattern),
                    FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
                    FilterRuleKind::Clear => FilterRuleSpec::clear(),
                    FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
                    FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
                    FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
                    FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
                });
            }
        }
        destination.extend(local);
    }
    Ok(())
}

/// Reports a filter file that could not be opened the way upstream's
/// `parse_filter_file` does, under upstream's exit code.
///
/// upstream: exclude.c:1703-1720 is ONE `if (!fp)` block with TWO arms selected
/// by `TEXT_FROM_FILE(template)`:
///
/// ```c
/// if (TEXT_FROM_FILE(template)) {
///         /* errno too: it answers "does this path exist". */
///         rprintf(FERROR, "failed to open %sclude file %s\n", ...,
///                 rule_text(template, fname));
/// } else {
///         rsyserr(FERROR, errno, "failed to open %sclude file %s", ..., fname);
/// }
/// exit_cleanup(RERR_FILEIO);
/// ```
///
/// So when the name came out of a file's contents, BOTH channels are withheld:
/// the path (via `rule_text`, which substitutes `<rule from FILE line N>`) and
/// `strerror(errno)`. The peer chooses what a merge file names, so errno would
/// answer "does this path exist, and may this process read it" for any path -
/// a filesystem probe through the filter parser. Only the TEXT differs between
/// the arms; both exit `RERR_FILEIO`, which is why the code is set once below.
///
/// The include/exclude wording follows `template->rflags & FILTRULE_INCLUDE` in
/// BOTH arms, so a merge rule carrying a `+` modifier (`.+ FILE`,
/// `merge,+ FILE`) says "include file" either way.
///
/// This rule is deliberately NOT shared with `--files-from`, whose own open
/// failure upstream reports from a different site and exits 1, not 11
/// (main.c:1886) - measured on 3.5.0.
fn filter_file_open_error(
    path: &Path,
    include: bool,
    error: &io::Error,
    source: RuleSource<'_>,
) -> Message {
    let word = if include { "in" } else { "ex" };
    let name = path.display().to_string();
    let text = if source.is_from_file() {
        format!(
            "failed to open {word}clude file {}",
            source.rule_text(&name)
        )
    } else {
        format!(
            "failed to open {word}clude file {name}: {}",
            upstream_io_error(error)
        )
    };
    rsync_error!(FILE_IO_EXIT_CODE, text).with_role(Role::Client)
}

/// Reads the raw pattern lines from a filter file, or from standard input when
/// `path` is `-`. `eol_nulls` selects NUL-delimited records (`--from0`).
/// `include` selects upstream's include/exclude wording if the open fails.
pub(crate) fn load_filter_file_patterns(
    path: &Path,
    eol_nulls: bool,
    include: bool,
) -> Result<Vec<String>, Message> {
    if path == Path::new("-") {
        return read_filter_patterns_from_standard_input(eol_nulls);
    }

    // `--include-from` / `--exclude-from` name the file on the command line, so
    // this is always upstream's non-file arm: the operator already knows the
    // path they typed, and errno is theirs to see.
    let file = File::open(path)
        .map_err(|error| filter_file_open_error(path, include, &error, RuleSource::Argument))?;

    let mut reader = BufReader::new(file);
    read_filter_patterns(&mut reader, eol_nulls).map_err(|error| {
        // Upstream has no analogue: its getc loop cannot distinguish a read
        // error from EOF, so a post-open failure silently ends the file. Keep
        // oc's louder report rather than inventing an upstream citation for it.
        let text = format!("failed to read filter file '{}': {error}", path.display());
        rsync_error!(1, text).with_role(Role::Client)
    })
}

/// Reads a merge file's entire contents as a string. `include` selects
/// upstream's include/exclude wording if the open fails; `source` says whether
/// this merge file was named by the operator or by another filter file's
/// contents, which selects the arm of `filter_file_open_error`.
pub(super) fn read_merge_file(
    path: &Path,
    include: bool,
    source: RuleSource<'_>,
) -> Result<String, Message> {
    // Opened separately from the read so that only a genuine open failure takes
    // upstream's wording and exit code; a mid-file read error (or non-UTF-8
    // contents) is a different condition and keeps its own report.
    let mut file =
        File::open(path).map_err(|error| filter_file_open_error(path, include, &error, source))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|error| {
        let text = format!("failed to read filter file '{}': {error}", path.display());
        rsync_error!(1, text).with_role(Role::Client)
    })?;
    Ok(contents)
}

/// Reads an entire merge file from standard input.
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

/// Reads filter pattern lines from standard input. `eol_nulls` selects
/// NUL-delimited records (`--from0`).
pub(crate) fn read_filter_patterns_from_standard_input(
    eol_nulls: bool,
) -> Result<Vec<String>, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        let mut cursor = io::Cursor::new(data);
        return read_filter_patterns(&mut cursor, eol_nulls).map_err(|error| {
            let text = format!("failed to read filter patterns from standard input: {error}");
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    read_filter_patterns(&mut reader, eol_nulls).map_err(|error| {
        let text = format!("failed to read filter patterns from standard input: {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

/// Splits a reader into filter pattern records, skipping blank lines and `#`/`;`
/// comments. Records split on newline, or on NUL when `eol_nulls` is set
/// (`--from0`), in which case embedded newlines are literal pattern bytes.
pub(super) fn read_filter_patterns<R: BufRead>(
    reader: &mut R,
    eol_nulls: bool,
) -> io::Result<Vec<String>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();

    // upstream: exclude.c:1501 parse_filter_file - `if (eol_nulls? !ch : (ch ==
    // '\n' || ch == '\r'))` splits records on NUL when --from0/-0 is set, so an
    // exclude-from/include-from file becomes NUL-delimited and embedded newlines
    // are literal pattern bytes (never stripped).
    let delimiter = if eol_nulls { b'\0' } else { b'\n' };

    loop {
        buffer.clear();
        let bytes_read = reader.read_until(delimiter, &mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&delimiter) {
            buffer.pop();
        }
        if !eol_nulls && buffer.last() == Some(&b'\r') {
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
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_skips_empty_lines() {
        let input = b"pattern1\n\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_hash_comments() {
        let input = b"pattern1\n# this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_semicolon_comments() {
        let input = b"pattern1\n; this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_crlf_line_endings() {
        let input = b"pattern1\r\npattern2\r\npattern3\r\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_handles_no_trailing_newline() {
        let input = b"pattern1\npattern2";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_handles_only_comments() {
        let input = b"# comment 1\n; comment 2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_handles_whitespace_only_lines() {
        let input = b"pattern1\n   \n\t\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_preserves_leading_whitespace() {
        let input = b"  pattern_with_leading_space\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, false).expect("read");
        assert_eq!(result, vec!["  pattern_with_leading_space"]);
    }

    #[test]
    fn load_filter_file_patterns_reads_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("filter.txt");
        std::fs::write(&path, "pattern1\npattern2\n").expect("write");
        let result = load_filter_file_patterns(&path, false, false).expect("load");
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    /// upstream: exclude.c:1712-1719 - a missing --exclude-from/--include-from
    /// file is fatal with RERR_FILEIO (11) and upstream's own wording, not the
    /// generic exit 1 this previously accepted.
    #[test]
    fn load_filter_file_patterns_reports_a_missing_file_like_upstream() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");

        let excluded = load_filter_file_patterns(&path, false, false).expect_err("must fail");
        assert_eq!(excluded.code(), Some(11));
        assert!(
            excluded.text().starts_with("failed to open exclude file "),
            "unexpected text: {}",
            excluded.text()
        );

        let included = load_filter_file_patterns(&path, false, true).expect_err("must fail");
        assert!(
            included.text().starts_with("failed to open include file "),
            "unexpected text: {}",
            included.text()
        );
    }

    #[test]
    fn read_merge_file_reads_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("merge.txt");
        std::fs::write(&path, "content here").expect("write");
        let result = read_merge_file(&path, false, RuleSource::Argument).expect("read");
        assert_eq!(result, "content here");
    }

    /// A merge rule naming a missing file is fatal under the same rule
    /// (upstream: exclude.c:1587 passes XFLG_FATAL_ERRORS for `merge`/`.`).
    ///
    /// This is the OPERATOR-named arm: the path they typed and `strerror` are
    /// both theirs to see (upstream's `rsyserr` branch, exclude.c:1714-1717).
    /// MEASURED on rsync 3.5.0: `--include-from=/definitely/missing` prints
    /// `failed to open include file /definitely/missing: No such file or
    /// directory (2)` and exits 11.
    #[test]
    fn read_merge_file_reports_a_missing_file_like_upstream() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");
        let error = read_merge_file(&path, false, RuleSource::Argument).expect_err("must fail");
        assert_eq!(error.code(), Some(11));
        assert!(
            error.text().starts_with("failed to open exclude file "),
            "unexpected text: {}",
            error.text()
        );
        assert!(
            error.text().contains("nonexistent.txt"),
            "the operator arm keeps the path they typed: {}",
            error.text()
        );
    }

    /// The FILE-named arm withholds BOTH channels: the path AND errno.
    ///
    /// upstream: exclude.c:1703-1720 is one `if (!fp)` block whose arms are
    /// selected by `TEXT_FROM_FILE(template)`. When the name came out of a
    /// file's contents it uses `rprintf` + `rule_text()` - no `strerror`,
    /// because errno answers "does this path exist, and may this process read
    /// it" for any path a peer chooses to name. Both arms exit `RERR_FILEIO`;
    /// only the TEXT differs, which is why the code is asserted equal here.
    ///
    /// MEASURED on rsync 3.5.0, a merge file containing `. /definitely/missing`:
    /// `failed to open exclude file <rule from FILE line 1>`, exit 11.
    #[test]
    fn read_merge_file_withholds_path_and_errno_when_the_name_came_from_a_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");
        let source = RuleSource::File {
            name: "outer.rules",
            line: 1,
        };
        let error = read_merge_file(&path, false, source).expect_err("must fail");

        // Same exit code as the operator arm - upstream never varies it.
        assert_eq!(error.code(), Some(11));
        assert!(
            error.text().contains("<rule from outer.rules line 1>"),
            "must name where the rule is: {}",
            error.text()
        );
        assert!(
            !error.text().contains("nonexistent.txt"),
            "must not echo the peer-chosen path: {}",
            error.text()
        );
        // The errno channel is the one a `rule_text`-only fix would leave open.
        assert!(
            !error.text().contains("No such file"),
            "must not leak strerror as an existence oracle: {}",
            error.text()
        );
    }

    #[test]
    fn read_filter_patterns_from_stdin_uses_test_input() {
        set_filter_stdin_input(b"stdin_pattern1\nstdin_pattern2\n".to_vec());
        let result = read_filter_patterns_from_standard_input(false).expect("read");
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
        let result = load_filter_file_patterns(Path::new("-"), false, false).expect("load");
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
            false,
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
            false,
        )
        .expect("append");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind(), FilterRuleKind::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn append_filter_rules_from_files_rejects_dir_merge() {
        let mut rules = Vec::new();
        let result =
            append_filter_rules_from_files(&mut rules, &[], FilterRuleKind::DirMerge, false);
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
            false,
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn read_filter_patterns_splits_on_nul_when_eol_nulls() {
        // upstream: exclude.c:1501 parse_filter_file - with eol_nulls (set by
        // --from0/-0) records are split on NUL, so embedded newlines are literal
        // pattern bytes and a NUL-delimited --exclude-from parses one rule per
        // NUL-separated record rather than one per line.
        let input = b"*.log\nwith space\0*.tmp\0";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, true).expect("read");
        assert_eq!(result, vec!["*.log\nwith space", "*.tmp"]);
    }

    #[test]
    fn read_filter_patterns_nul_mode_handles_no_trailing_nul() {
        // upstream: exclude.c:1516-1517 - the final record before EOF is parsed
        // even without a trailing delimiter.
        let input = b"only\0last";
        let mut reader = Cursor::new(input.to_vec());
        let result = read_filter_patterns(&mut reader, true).expect("read");
        assert_eq!(result, vec!["only", "last"]);
    }

    #[test]
    fn append_filter_rules_from_files_honors_from0_nul_split() {
        // upstream: exclude.c:1501 parse_filter_file - --exclude-from with
        // --from0 reads NUL-delimited records; "a\nb" is one literal pattern.
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("exclude0.txt");
        std::fs::write(&path, b"a\nb\0*.bak\0").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Exclude,
            true,
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "a\nb");
        assert_eq!(rules[1].pattern(), "*.bak");
    }
}
