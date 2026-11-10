use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use crate::client::{FILE_IO_EXIT_CODE, FilterRuleKind, FilterRuleSpec};
use crate::message::{Message, Role};
use crate::rsync_error;
use engine::local_copy::upstream_io_error;

use filters::RuleSource;

use super::directive::FilterDirective;
use super::filter_str::{Xflags, add_rule};
use super::parsing::parse_old_prefix_rule;
use super::{FilterFileKind, FilterFileSource};

/// Loads filter patterns from `--include-from` / `--exclude-from` files and
/// appends the resulting rules to `destination`. Include/exclude files honor the
/// old-prefix syntax (`- `/`+ `/`!`), and a `!` empties the whole list;
/// dir-merge is rejected. `eol_nulls` selects NUL-delimited records (`--from0`),
/// and `xflags` are the flags the rules are added with (exclude.c:279-314).
pub(crate) fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    names: &[OsString],
    kind: FilterRuleKind,
    eol_nulls: bool,
    files: &dyn FilterFileSource,
    xflags: Xflags,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    // upstream: options.c:1635-1657 - --exclude-from and --include-from
    // feed parse_filter_file with XFLG_OLD_PREFIXES so per-line `- pat`,
    // `+ pat`, and `!` prefixes flip the rule kind or clear the list. Kinds
    // other than Include/Exclude have no upstream OLD_PREFIXES analogue, so
    // we keep the legacy raw-pattern behavior to preserve existing semantics.
    let honor_old_prefixes = matches!(kind, FilterRuleKind::Include | FilterRuleKind::Exclude);

    // upstream: exclude.c:1542-1551 - a `!` read from the file runs
    // `pop_filter_list(listp); listp->head = NULL`, and parse_filter_file()
    // parses into the caller's own list, so the clear empties every rule
    // ahead of it, argv rules included.
    for path in names {
        let name = path.to_string_lossy();
        let patterns = load_filter_file_patterns(
            Path::new(path.as_os_str()),
            eol_nulls,
            matches!(kind, FilterRuleKind::Include),
            files,
            destination,
        )?;
        for (pattern, line) in patterns {
            if honor_old_prefixes {
                // upstream: exclude.c:1749-1761 - these are the CONTENTS of a
                // file, so `rule_src_file` is set for the whole read and a bad
                // record is reported as `<rule from FILE line N>`, never echoed
                // (exclude.c:71-86).
                let source = RuleSource::File { name: &name, line };
                match parse_old_prefix_rule(&pattern, kind, source)? {
                    FilterDirective::Rule(rule) => add_rule(destination, rule, xflags),
                    FilterDirective::Clear => destination.clear(),
                    // A blank line in an exclude-from/include-from file is skipped.
                    FilterDirective::Noop => {}
                    FilterDirective::Merge(_) | FilterDirective::CvsDefaults(_) => {
                        unreachable!(
                            "parse_old_prefix_rule never emits FilterDirective::Merge or CvsDefaults"
                        )
                    }
                }
            } else {
                destination.push(match kind {
                    FilterRuleKind::Include => FilterRuleSpec::include(pattern),
                    FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
                    FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
                    FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
                    FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
                });
            }
        }
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
/// (main.c:1913) - measured on 3.5.1.
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
/// `path` is `-`, each with its line number (see [`read_filter_patterns`]).
/// `eol_nulls` selects NUL-delimited records (`--from0`). `include` selects
/// upstream's include/exclude wording if the open fails. `list` is the list
/// built so far, which the open may consult.
pub(crate) fn load_filter_file_patterns(
    path: &Path,
    eol_nulls: bool,
    include: bool,
    files: &dyn FilterFileSource,
    list: &[FilterRuleSpec],
) -> Result<Vec<(String, usize)>, Message> {
    // `--include-from` / `--exclude-from` name the file on the command line, so
    // this is always upstream's non-file arm: the operator already knows the
    // path they typed, and errno is theirs to see.
    let reader = files
        .open(path, FilterFileKind::RuleList, list)
        .map_err(|error| filter_file_open_error(path, include, &error, RuleSource::Argument))?;
    let mut reader = BufReader::new(reader);
    read_filter_patterns(&mut reader, eol_nulls).map_err(|error| read_failure(path, &error))
}

/// Reads a merge file's entire contents as a string. `include` selects
/// upstream's include/exclude wording if the open fails; `source` says whether
/// this merge file was named by the operator or by another filter file's
/// contents, which selects the arm of `filter_file_open_error`.
pub(super) fn read_merge_file(
    path: &Path,
    include: bool,
    source: RuleSource<'_>,
    files: &dyn FilterFileSource,
    list: &[FilterRuleSpec],
) -> Result<String, Message> {
    // Opened separately from the read so that only a genuine open failure takes
    // upstream's wording and exit code; a mid-file read error (or non-UTF-8
    // contents) is a different condition and keeps its own report.
    let mut reader = files
        .open(path, FilterFileKind::MergeFile, list)
        .map_err(|error| filter_file_open_error(path, include, &error, source))?;
    let mut contents = String::new();
    reader
        .read_to_string(&mut contents)
        .map_err(|error| read_failure(path, &error))?;
    Ok(contents)
}

/// Reports a filter file that opened but could not be read.
///
/// Upstream has no analogue: its getc loop cannot distinguish a read error
/// from EOF, so a post-open failure silently ends the file. Keep oc's louder
/// report rather than inventing an upstream citation for it.
fn read_failure(path: &Path, error: &io::Error) -> Message {
    let text = if path == Path::new("-") {
        format!("failed to read filter patterns from standard input: {error}")
    } else {
        format!("failed to read filter file '{}': {error}", path.display())
    };
    rsync_error!(1, text).with_role(Role::Client)
}

/// Splits a reader into filter pattern records, skipping EMPTY records and
/// those whose FIRST BYTE is `#`/`;` - the test
/// [`filters::filter_file_line_is_rule`] owns. Nothing is trimmed, so a
/// whitespace-only record and an indented `#` are patterns. Records split at
/// `\n`, at `\r`, or at `\r\n` - the boundary
/// [`filters::filter_file_records`] owns - or on NUL when `eol_nulls` is set
/// (`--from0`), in which case embedded newlines are literal pattern bytes.
///
/// Each pattern is paired with its 1-based record number: upstream's
/// `rule_src_line` counts every record read (exclude.c:1759-1761), before the
/// comment and empty-record test, so a bad rule is reported at the line an
/// editor shows.
pub(super) fn read_filter_patterns<R: BufRead>(
    reader: &mut R,
    eol_nulls: bool,
) -> io::Result<Vec<(String, usize)>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();
    let mut line = 0;

    // upstream: exclude.c:1774 parse_filter_file - `if (eol_nulls? !ch : (ch ==
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

        // upstream: exclude.c:1806. `filters::filter_file_line_is_rule` is the
        // single owner of that test; it looks at the FIRST BYTE and does not
        // trim. `--*clude-from` files are always line-parsed, never word-split.
        // MEASURED against rsync 3.5.1: an `--exclude-from` line of `   ` (three
        // spaces) or `  #a` excludes a file of exactly that name.
        let chunk = String::from_utf8_lossy(&buffer);

        if eol_nulls {
            let mut record = chunk.into_owned();
            if record.as_bytes().last() == Some(&b'\0') {
                record.pop();
            }
            line += 1;
            if filters::filter_file_line_is_rule(&record, true) {
                patterns.push((record, line));
            }
            continue;
        }

        // `read_until(b'\n')` is only a buffering boundary: a lone `\r` also
        // ends a record (exclude.c:1774-1793), so the chunk is re-split by the
        // single owner `filters::filter_file_records`, terminator and all.
        for record in filters::filter_file_records(&chunk) {
            line += 1;
            if filters::filter_file_line_is_rule(record, true) {
                patterns.push((record.to_owned(), line));
            }
        }
    }

    Ok(patterns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::filter_args::{TestFiles, set_filter_stdin_input};
    use std::io::Cursor;

    /// The pattern texts of numbered records.
    fn texts(records: Vec<(String, usize)>) -> Vec<String> {
        records.into_iter().map(|(pattern, _)| pattern).collect()
    }

    #[test]
    fn read_filter_patterns_parses_simple_lines() {
        let input = b"pattern1\npattern2\npattern3\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_skips_empty_lines() {
        let input = b"pattern1\n\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_hash_comments() {
        let input = b"pattern1\n# this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_skips_semicolon_comments() {
        let input = b"pattern1\n; this is a comment\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_crlf_line_endings() {
        let input = b"pattern1\r\npattern2\r\npattern3\r\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2", "pattern3"]);
    }

    #[test]
    fn read_filter_patterns_handles_no_trailing_newline() {
        let input = b"pattern1\npattern2";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_handles_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_handles_only_comments() {
        let input = b"# comment 1\n; comment 2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert!(result.is_empty());
    }

    #[test]
    fn read_filter_patterns_keeps_whitespace_only_lines() {
        // upstream: exclude.c:1806 tests `*line`, the first BYTE, so a
        // whitespace-only record is not empty and becomes a literal pattern.
        // MEASURED against rsync 3.5.1: an `--exclude-from` file holding only
        // `   ` excludes a file literally named `   `.
        let input = b"pattern1\n   \n\t\npattern2\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["pattern1", "   ", "\t", "pattern2"]);
    }

    #[test]
    fn read_filter_patterns_keeps_an_indented_comment_marker() {
        // upstream: exclude.c:1806 - only a `;`/`#` in COLUMN ZERO is a
        // comment. MEASURED against rsync 3.5.1: an `--exclude-from` file
        // holding only `  #a` excludes a file literally named `  #a`.
        let input = b"  #a\n  ;b\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["  #a", "  ;b"]);
    }

    #[test]
    fn read_filter_patterns_preserves_leading_whitespace() {
        let input = b"  pattern_with_leading_space\n";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, false).expect("read"));
        assert_eq!(result, vec!["  pattern_with_leading_space"]);
    }

    #[test]
    fn load_filter_file_patterns_reads_file() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("filter.txt");
        std::fs::write(&path, "pattern1\npattern2\n").expect("write");
        let result =
            texts(load_filter_file_patterns(&path, false, false, &TestFiles, &[]).expect("load"));
        assert_eq!(result, vec!["pattern1", "pattern2"]);
    }

    /// upstream: exclude.c:1703-1719 - a missing --exclude-from/--include-from
    /// file is fatal with RERR_FILEIO (11) and upstream's own wording.
    #[test]
    fn load_filter_file_patterns_reports_a_missing_file_like_upstream() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("nonexistent.txt");

        let excluded =
            load_filter_file_patterns(&path, false, false, &TestFiles, &[]).expect_err("must fail");
        assert_eq!(excluded.code(), Some(11));
        assert!(
            excluded.text().starts_with("failed to open exclude file "),
            "unexpected text: {}",
            excluded.text()
        );

        let included =
            load_filter_file_patterns(&path, false, true, &TestFiles, &[]).expect_err("must fail");
        assert!(
            included.text().starts_with("failed to open include file "),
            "unexpected text: {}",
            included.text()
        );
    }

    #[test]
    fn read_merge_file_reads_file() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("merge.txt");
        std::fs::write(&path, "content here").expect("write");
        let result =
            read_merge_file(&path, false, RuleSource::Argument, &TestFiles, &[]).expect("read");
        assert_eq!(result, "content here");
    }

    /// A merge rule naming a missing file is fatal under the same rule
    /// (upstream: exclude.c:1587 passes XFLG_FATAL_ERRORS for `merge`/`.`).
    ///
    /// This is the OPERATOR-named arm: the path they typed and `strerror` are
    /// both theirs to see (upstream's `rsyserr` branch, exclude.c:1714-1717).
    /// MEASURED on rsync 3.5.1: `--include-from=/definitely/missing` prints
    /// `failed to open include file /definitely/missing: No such file or
    /// directory (2)` and exits 11.
    #[test]
    fn read_merge_file_reports_a_missing_file_like_upstream() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("nonexistent.txt");
        let error = read_merge_file(&path, false, RuleSource::Argument, &TestFiles, &[])
            .expect_err("must fail");
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
    /// MEASURED on rsync 3.5.1, a merge file containing `. /definitely/missing`:
    /// `failed to open exclude file <rule from FILE line 1>`, exit 11.
    #[test]
    fn read_merge_file_withholds_path_and_errno_when_the_name_came_from_a_file() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("nonexistent.txt");
        let source = RuleSource::File {
            name: "outer.rules",
            line: 1,
        };
        let error = read_merge_file(&path, false, source, &TestFiles, &[]).expect_err("must fail");

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
        let result = texts(
            load_filter_file_patterns(Path::new("-"), false, false, &TestFiles, &[]).expect("read"),
        );
        assert_eq!(result, vec!["stdin_pattern1", "stdin_pattern2"]);
    }

    #[test]
    fn read_merge_from_stdin_uses_test_input() {
        set_filter_stdin_input(b"stdin content here".to_vec());
        let result = read_merge_file(Path::new("-"), false, RuleSource::Argument, &TestFiles, &[])
            .expect("read");
        assert_eq!(result, "stdin content here");
    }

    #[test]
    fn load_filter_file_patterns_handles_dash_path() {
        set_filter_stdin_input(b"stdin_pattern\n".to_vec());
        let result = texts(
            load_filter_file_patterns(Path::new("-"), false, false, &TestFiles, &[]).expect("load"),
        );
        assert_eq!(result, vec!["stdin_pattern"]);
    }

    #[test]
    fn append_filter_rules_from_files_adds_include_rules() {
        let temp = test_support::create_tempdir();
        let path = temp.path().join("include.txt");
        std::fs::write(&path, "*.rs\n*.toml\n").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Include,
            false,
            &TestFiles,
            Xflags::default(),
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
        let temp = test_support::create_tempdir();
        let path = temp.path().join("exclude.txt");
        std::fs::write(&path, "*.bak\n").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Exclude,
            false,
            &TestFiles,
            Xflags::default(),
        )
        .expect("append");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind(), FilterRuleKind::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn append_filter_rules_from_files_rejects_dir_merge() {
        let mut rules = Vec::new();
        let result = append_filter_rules_from_files(
            &mut rules,
            &[],
            FilterRuleKind::DirMerge,
            false,
            &TestFiles,
            Xflags::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn append_filter_rules_from_files_handles_multiple_files() {
        let temp = test_support::create_tempdir();
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
            &TestFiles,
            Xflags::default(),
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn read_filter_patterns_splits_on_nul_when_eol_nulls() {
        // upstream: exclude.c:1774 parse_filter_file - with eol_nulls (set by
        // --from0/-0) records are split on NUL, so embedded newlines are literal
        // pattern bytes and a NUL-delimited --exclude-from parses one rule per
        // NUL-separated record rather than one per line.
        let input = b"*.log\nwith space\0*.tmp\0";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, true).expect("read"));
        assert_eq!(result, vec!["*.log\nwith space", "*.tmp"]);
    }

    #[test]
    fn read_filter_patterns_nul_mode_handles_no_trailing_nul() {
        // upstream: exclude.c:1806-1810 - the final record before EOF is parsed
        // even without a trailing delimiter.
        let input = b"only\0last";
        let mut reader = Cursor::new(input.to_vec());
        let result = texts(read_filter_patterns(&mut reader, true).expect("read"));
        assert_eq!(result, vec!["only", "last"]);
    }

    #[test]
    fn append_filter_rules_from_files_honors_from0_nul_split() {
        // upstream: exclude.c:1774 parse_filter_file - --exclude-from with
        // --from0 reads NUL-delimited records; "a\nb" is one literal pattern.
        let temp = test_support::create_tempdir();
        let path = temp.path().join("exclude0.txt");
        std::fs::write(&path, b"a\nb\0*.bak\0").expect("write");
        let mut rules = Vec::new();
        append_filter_rules_from_files(
            &mut rules,
            &[OsString::from(path)],
            FilterRuleKind::Exclude,
            true,
            &TestFiles,
            Xflags::default(),
        )
        .expect("append");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern(), "a\nb");
        assert_eq!(rules[1].pattern(), "*.bak");
    }

    /// The ownership walk must still FOLLOW a symlink whose components we own.
    ///
    /// Upstream refuses only a component owned by a uid that is neither root
    /// nor our euid (`syscall.c:499`), so a filter file reached through a
    /// symlink the operator created is read normally. This is the
    /// over-refusal direction: it is what a walk that simply refused every
    /// symlink would break, and unlike the cross-uid refusal it needs no root
    /// to observe, so it runs on every platform the suite covers.
    #[cfg(unix)]
    #[test]
    fn a_self_owned_symlink_to_a_filter_file_is_still_read() {
        let dir = test_support::create_tempdir();
        let target = dir.path().join("real.rules");
        std::fs::write(&target, "*.bak\n").expect("write");
        let link = dir.path().join("link.rules");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let patterns = load_filter_file_patterns(&link, false, false, &TestFiles, &[])
            .expect("a self-owned symlink must be followed, not refused");
        assert_eq!(texts(patterns), vec!["*.bak"]);
    }
}
