//! Wildcard expansion of local source operands for native Windows.
//!
//! On Unix the shell expands `*`, `?` and `[...]` before rsync ever sees its
//! argv; upstream rsync has no client-side globbing of its own. `cmd.exe` and
//! PowerShell pass such operands through literally, so on Windows oc-rsync
//! performs the expansion a POSIX shell would have done:
//!
//! - only local source operands are expanded - never the destination (the
//!   last operand) and never a remote `host:path` or `rsync://` operand;
//! - `*` and `?` never match a leading `.`, which must be named explicitly;
//! - matches are sorted by byte value (the `C` locale collation);
//! - a pattern that matches nothing is kept literally, so the transfer then
//!   reports the usual missing-source error;
//! - a trailing separator restricts matches to directories and is kept on
//!   every result.
//!
//! `\` is a path separator on Windows, so it cannot escape a metacharacter.
//! A literal `[` is written as the bracket expression `[[]`.

use std::ffi::OsString;
use std::fs;

use super::operand_is_remote;

/// Expands the wildcard patterns among the local source operands.
///
/// The last operand is the destination and is returned untouched, as is a
/// lone operand. Operands that are remote, not valid Unicode, or free of
/// metacharacters pass through unchanged.
pub(crate) fn expand_source_operands(operands: Vec<OsString>) -> Vec<OsString> {
    let last = operands.len().saturating_sub(1);
    let mut expanded = Vec::with_capacity(operands.len());
    for (index, operand) in operands.into_iter().enumerate() {
        match operand.to_str() {
            Some(pattern) if index < last && !operand_is_remote(&operand) => {
                expanded.extend(expand_pattern(pattern).into_iter().map(OsString::from));
            }
            _ => expanded.push(operand),
        }
    }
    expanded
}

/// Expands one pattern against the filesystem, returning the sorted matches,
/// or the pattern itself when nothing matches.
fn expand_pattern(pattern: &str) -> Vec<String> {
    if !pattern.contains(['*', '?', '[']) {
        return vec![pattern.to_owned()];
    }
    let (root, mut rest) = split_root(pattern);
    let mut candidates = vec![root.to_owned()];
    let mut dirs_only = false;
    while !candidates.is_empty() {
        let after_seps = rest.trim_start_matches(is_separator);
        let seps = &rest[..rest.len() - after_seps.len()];
        if after_seps.is_empty() {
            if !seps.is_empty() {
                dirs_only = true;
                for candidate in &mut candidates {
                    candidate.push_str(seps);
                }
            }
            break;
        }
        let end = after_seps.find(is_separator).unwrap_or(after_seps.len());
        let segment = &after_seps[..end];
        rest = &after_seps[end..];
        candidates = match segment_pattern(segment) {
            Some(glob) => candidates
                .iter()
                .flat_map(|parent| matching_children(&format!("{parent}{seps}"), segment, &glob))
                .collect(),
            None => candidates
                .into_iter()
                .map(|parent| format!("{parent}{seps}{segment}"))
                .collect(),
        };
    }
    candidates.retain(|path| {
        if dirs_only {
            fs::metadata(path).is_ok_and(|meta| meta.is_dir())
        } else {
            fs::symlink_metadata(path).is_ok()
        }
    });
    if candidates.is_empty() {
        return vec![pattern.to_owned()];
    }
    candidates.sort_unstable();
    candidates
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Splits off a prefix that must never be globbed: the `?` of a `\\?\`
/// verbatim prefix is not a wildcard, and a `C:` drive designator is not part
/// of the first segment's name.
fn split_root(pattern: &str) -> (&str, &str) {
    let b = pattern.as_bytes();
    let sep = |i: usize| b.get(i).is_some_and(|&c| c == b'/' || c == b'\\');
    if sep(0) && sep(1) && matches!(b.get(2), Some(b'?' | b'.')) && sep(3) {
        pattern.split_at(4)
    } else if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        pattern.split_at(2)
    } else {
        ("", pattern)
    }
}

/// Lists the entries of `dir` (`""` meaning the working directory) whose name
/// matches `segment`, joined onto `dir`.
fn matching_children(dir: &str, segment: &str, glob: &[u8]) -> Vec<String> {
    let Ok(entries) = fs::read_dir(if dir.is_empty() { "." } else { dir }) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name_matches(segment, glob, name))
        .map(|name| format!("{dir}{name}"))
        .collect()
}

/// POSIX pathname expansion: a leading `.` is matched only by a pattern that
/// itself starts with a literal `.`.
fn name_matches(segment: &str, glob: &[u8], name: &str) -> bool {
    (!name.starts_with('.') || segment.starts_with('.'))
        && filters::wildmatch(glob, name.as_bytes())
}

/// Returns the `wildmatch` form of `segment`, or `None` when the segment has
/// no wildcard and names a single entry literally.
///
/// A `[` without a closing `]` is an ordinary character to a POSIX shell but
/// aborts `wildmatch`, so it is escaped. Segments never contain `\` (it is a
/// separator), so the escape cannot collide with user input.
fn segment_pattern(segment: &str) -> Option<Vec<u8>> {
    let bytes = segment.as_bytes();
    let mut glob = Vec::with_capacity(bytes.len() + 2);
    let mut wild = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'*' | b'?' => wild = true,
            b'[' => match bracket_end(bytes, i) {
                Some(end) => {
                    wild = true;
                    glob.extend_from_slice(&bytes[i..=end]);
                    i = end + 1;
                    continue;
                }
                None => glob.push(b'\\'),
            },
            _ => {}
        }
        glob.push(bytes[i]);
        i += 1;
    }
    wild.then_some(glob)
}

/// Index of the `]` closing the bracket expression opened at `open`. A `]`
/// directly after `[`, `[!` or `[^` is a member, not the terminator.
fn bracket_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    if matches!(bytes.get(i), Some(b'!' | b'^')) {
        i += 1;
    }
    if bytes.get(i) == Some(&b']') {
        i += 1;
    }
    bytes[i.min(bytes.len())..]
        .iter()
        .position(|&c| c == b']')
        .map(|offset| i + offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Builds a tree of empty files (and directories for names ending in
    /// `/`) and returns it with its root as a `/`-joined pattern prefix.
    fn tree(entries: &[&str]) -> (TempDir, String) {
        let tmp = tempfile::tempdir().expect("tempdir");
        for entry in entries {
            let path = tmp.path().join(entry);
            if entry.ends_with('/') {
                fs::create_dir_all(&path).expect("mkdir");
            } else {
                fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
                fs::write(&path, b"").expect("write");
            }
        }
        let root = tmp.path().to_str().expect("utf-8 tempdir").to_owned();
        (tmp, root)
    }

    /// Strips the tempdir prefix so expectations read as relative names.
    fn rel(root: &str, matches: Vec<String>) -> Vec<String> {
        matches
            .into_iter()
            .map(|m| m.strip_prefix(&format!("{root}/")).unwrap_or(&m).to_owned())
            .collect()
    }

    #[test]
    fn star_matches_are_sorted_by_byte_value() {
        // The C locale orders upper case before lower case; a case-folding or
        // directory-order result would make transfers nondeterministic. The
        // names differ by more than case so they stay distinct files on
        // case-insensitive filesystems (NTFS, default APFS).
        let (_t, root) = tree(&["b.txt", "a.txt", "C.txt", "c.log"]);
        let got = rel(&root, expand_pattern(&format!("{root}/*.txt")));
        assert_eq!(got, ["C.txt", "a.txt", "b.txt"]);
    }

    #[test]
    fn unmatched_pattern_is_kept_literally() {
        // The literal operand must reach the transfer so it fails with the
        // normal missing-source error, as it does after a POSIX shell.
        let (_t, root) = tree(&["a.txt"]);
        let pattern = format!("{root}/*.nope");
        assert_eq!(expand_pattern(&pattern), [pattern]);
    }

    #[test]
    fn pattern_without_metacharacters_is_returned_unchecked() {
        let pattern = "/no/such/dir/file.txt";
        assert_eq!(expand_pattern(pattern), [pattern]);
    }

    #[test]
    fn leading_dot_must_be_matched_explicitly() {
        let (_t, root) = tree(&[".hidden", "visible"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/*"))),
            ["visible"]
        );
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/?hidden"))),
            ["?hidden"]
        );
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/.*"))),
            [".hidden"]
        );
    }

    #[test]
    fn question_mark_and_bracket_expressions() {
        let (_t, root) = tree(&["a1", "a2", "b1", "ab1"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/?1"))),
            ["a1", "b1"]
        );
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/a[0-1]"))),
            ["a1"]
        );
        assert_eq!(rel(&root, expand_pattern(&format!("{root}/[!a]1"))), ["b1"]);
        assert_eq!(rel(&root, expand_pattern(&format!("{root}/[^a]1"))), ["b1"]);
    }

    #[test]
    fn bracket_expression_escapes_a_literal_bracket() {
        // `\` is a separator on Windows, so `[[]` is the only way to name a `[`.
        let (_t, root) = tree(&["[x]", "x]"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/[[]x]"))),
            ["[x]"]
        );
    }

    #[test]
    fn unterminated_bracket_is_an_ordinary_character() {
        let (_t, root) = tree(&["a[b1", "a[b2", "ab1"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/a[b*"))),
            ["a[b1", "a[b2"]
        );
    }

    #[test]
    fn trailing_separator_matches_only_directories_and_is_kept() {
        let (_t, root) = tree(&["d1/", "d2"]);
        assert_eq!(rel(&root, expand_pattern(&format!("{root}/d*/"))), ["d1/"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/d*"))),
            ["d1", "d2"]
        );
    }

    #[test]
    fn wildcard_in_a_directory_segment_requires_the_literal_tail() {
        let (_t, root) = tree(&["a/f.txt", "b/g.txt", "c"]);
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/*/f.txt"))),
            ["a/f.txt"]
        );
        assert_eq!(
            rel(&root, expand_pattern(&format!("{root}/*/*.txt"))),
            ["a/f.txt", "b/g.txt"]
        );
    }

    #[test]
    fn separators_are_reproduced_as_written() {
        let (_t, root) = tree(&["a.txt"]);
        assert_eq!(
            expand_pattern(&format!("{root}//*.txt")),
            [format!("{root}//a.txt")]
        );
    }

    #[test]
    fn root_prefix_is_never_globbed() {
        assert_eq!(split_root(r"\\?\C:\d\*"), (r"\\?\", r"C:\d\*"));
        assert_eq!(split_root(r"\\.\pipe\*"), (r"\\.\", r"pipe\*"));
        assert_eq!(split_root("C:*.txt"), ("C:", "*.txt"));
        assert_eq!(split_root(r"\\srv\share\*"), ("", r"\\srv\share\*"));
        assert_eq!(split_root("*.txt"), ("", "*.txt"));
    }

    #[test]
    fn segment_pattern_classifies_wildcards() {
        assert_eq!(segment_pattern("abc"), None);
        assert_eq!(segment_pattern("a[b"), None);
        assert_eq!(segment_pattern("a[b*").as_deref(), Some(&b"a\\[b*"[..]));
        assert_eq!(segment_pattern("[]a]").as_deref(), Some(&b"[]a]"[..]));
        assert_eq!(
            segment_pattern("[[:digit:]]").as_deref(),
            Some(&b"[[:digit:]]"[..])
        );
    }

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_local_sources_are_expanded() {
        let (_t, root) = tree(&["a.txt", "b.txt"]);
        let pattern = format!("{root}/*.txt");
        let got = expand_source_operands(os(&[&pattern, "host:*.txt", &pattern]));
        let want = os(&[
            &format!("{root}/a.txt"),
            &format!("{root}/b.txt"),
            "host:*.txt",
            &pattern,
        ]);
        assert_eq!(got, want, "remote source and destination must stay literal");
    }

    #[test]
    fn lone_operand_is_not_expanded() {
        // Expanding it to several paths would turn the last match into a
        // destination and overwrite it.
        let (_t, root) = tree(&["a.txt", "b.txt"]);
        let pattern = format!("{root}/*.txt");
        assert_eq!(expand_source_operands(os(&[&pattern])), os(&[&pattern]));
    }

    #[test]
    fn expansion_keeps_operand_order() {
        let (_t, root) = tree(&["x/2", "x/1", "y"]);
        let got =
            expand_source_operands(os(&[&format!("{root}/y"), &format!("{root}/x/*"), "dest"]));
        let want = os(&[
            &format!("{root}/y"),
            &format!("{root}/x/1"),
            &format!("{root}/x/2"),
            "dest",
        ]);
        assert_eq!(got, want);
    }
}
