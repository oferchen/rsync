use crate::error::TaskResult;
use crate::util::{list_rust_sources_via_git, validation_error};
use std::fs;
use std::io::BufRead;
use std::path::Path;

const TODO_MACRO: &[u8] = b"todo!";
const UNIMPLEMENTED_MACRO: &[u8] = b"unimplemented!";
const MARKER_WORDS: [&[u8]; 4] = [b"todo", b"unimplemented", b"fixme", b"xxx"];

/// The scanner's own source file, exempt from the scan. The marker literals
/// here are pattern definitions, test fixtures and the violation message -
/// a gate that flags its own implementation cannot pass on the tree it
/// guards.
const SCANNER_SOURCE: &str = "xtask/src/commands/no_placeholders.rs";

/// Executes the `no-placeholders` command.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let mut violations_present = false;
    let rust_files = list_rust_sources_via_git(workspace)?;

    for relative in rust_files {
        if is_exempt_source(&relative) {
            continue;
        }

        let absolute = workspace.join(&relative);
        let findings = scan_rust_file_for_placeholders(&absolute)?;
        if findings.is_empty() {
            continue;
        }

        violations_present = true;
        for finding in findings {
            eprintln!(
                "{}:{}:{}",
                relative.display(),
                finding.line,
                finding.snippet
            );
        }
    }

    if violations_present {
        return Err(validation_error(concat!(
            "placeholder markers detected in Rust sources; remove todo!/unimplemented! ",
            "macros and TODO:/FIXME:/XXX-style annotations"
        )));
    }

    Ok(())
}

/// Returns whether `relative` (a git-reported workspace-relative path) is the
/// scanner's own source file.
fn is_exempt_source(relative: &Path) -> bool {
    relative == Path::new(SCANNER_SOURCE)
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PlaceholderFinding {
    line: usize,
    snippet: String,
}

fn scan_rust_file_for_placeholders(path: &Path) -> TaskResult<Vec<PlaceholderFinding>> {
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = String::new();
    let mut findings = Vec::new();
    let mut line_number = 0usize;

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }

        line_number += 1;

        let line = buffer.trim_end_matches(['\r', '\n']);
        if contains_placeholder(line) {
            findings.push(PlaceholderFinding {
                line: line_number,
                snippet: line.to_owned(),
            });
        }
    }

    Ok(findings)
}

fn contains_placeholder(line: &str) -> bool {
    let line_bytes = line.as_bytes();
    if contains_subsequence(line_bytes, TODO_MACRO)
        || contains_subsequence(line_bytes, UNIMPLEMENTED_MACRO)
    {
        return true;
    }

    MARKER_WORDS
        .iter()
        .any(|word| contains_marker_annotation(line_bytes, word))
}

fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }

    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Finds a standalone marker word used as an annotation.
///
/// A marker word counts only when it is written in annotation syntax -
/// immediately followed by `:` or `(` (`TODO: fix`, `FIXME(name): later`).
/// A bare word is prose or notation, not a work marker: the `-e.xxx`
/// capability-string shorthand and comments like "unimplemented FUSE mknod"
/// describe code that exists. An identifier-cased word (`Todo` in
/// `XattrState::Todo`, mirroring upstream's XSTATE_TODO) is a name, not a
/// marker, and stays exempt even in annotation position (`Todo(3)`,
/// `Todo::is_set()`). SCREAMING_CASE identifiers such as `XSTATE_TODO` need
/// no arm of their own: `_` is an identifier byte, so the word-boundary
/// check already rejects them.
fn contains_marker_annotation(haystack: &[u8], word: &[u8]) -> bool {
    let mut index = 0usize;
    while index + word.len() <= haystack.len() {
        let candidate = &haystack[index..index + word.len()];
        if candidate.eq_ignore_ascii_case(word) {
            let before_ok = index == 0 || !is_identifier_byte(haystack[index - 1]);
            let after = haystack.get(index + word.len()).copied();
            let annotation = matches!(after, Some(b':') | Some(b'('));

            if before_ok && annotation && !is_identifier_cased(candidate) {
                return true;
            }
        }

        index += 1;
    }

    false
}

const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Returns whether `word` is written as a Pascal-cased identifier segment:
/// a leading uppercase letter with no other uppercase (`Todo`, not `TODO`
/// or `todo`).
fn is_identifier_cased(word: &[u8]) -> bool {
    match word.split_first() {
        Some((first, rest)) => {
            first.is_ascii_uppercase() && !rest.iter().any(u8::is_ascii_uppercase)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_temp_path(suffix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("rsync_xtask_{now}_{suffix}"))
    }

    fn scan_content(suffix: &str, content: &str) -> Vec<PlaceholderFinding> {
        let path = unique_temp_path(suffix);
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        findings
    }

    #[test]
    fn scan_detects_todo_macro() {
        let findings = scan_content("todo_macro", "fn example() {\n    todo!();\n}\n");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].snippet.contains("todo!"));
    }

    #[test]
    fn scan_detects_unimplemented_macro() {
        let findings = scan_content(
            "unimplemented_macro",
            "fn example() {\n    unimplemented!()\n}\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn scan_detects_fixme_comment() {
        let findings = scan_content(
            "fixme_comment",
            "// header\n// FIXME: implement\nfn ready() {}\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn scan_detects_todo_comment() {
        let findings = scan_content(
            "todo_comment",
            "// TODO: fill in implementation\nfn stub() {}\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 1);
    }

    #[test]
    fn scan_detects_first_line_placeholder() {
        let findings = scan_content("first_line_placeholder", "// FIXME: license\nfn ok() {}\n");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 1);
    }

    #[test]
    fn scan_detects_placeholder_inside_multiline_panic() {
        let findings = scan_content(
            "panic_multiline",
            "fn explode() {\n    panic!(\n        \"TODO: revisit\"\n    );\n}\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
    }

    #[test]
    fn scan_detects_attributed_annotation() {
        let findings = scan_content("attributed", "// TODO(alice): later\n");
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn bare_marker_word_is_prose_not_a_marker() {
        let findings = scan_content(
            "bare_words",
            concat!(
                "// the `-e.xxx` capability string advertises features\n",
                "// unimplemented FUSE mknod stays a warning\n",
                "let name = \"TODO.txt\";\n",
                "let payload = b\"xxx\";\n",
            ),
        );
        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn identifier_cased_word_is_a_name_not_a_marker() {
        let findings = scan_content(
            "identifier_cased",
            concat!(
                "assert_eq!(entry.state(), XattrState::Todo);\n",
                "let wrapped = Todo(3);\n",
                "let picked = Todo::default();\n",
            ),
        );
        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn screaming_case_identifier_is_not_standalone() {
        let findings = scan_content("screaming", "const XSTATE_TODO: u8 = 3;\n");
        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn scanner_source_is_the_only_exempt_file() {
        assert!(is_exempt_source(Path::new(
            "xtask/src/commands/no_placeholders.rs"
        )));
        assert!(!is_exempt_source(Path::new(
            "xtask/src/commands/citations.rs"
        )));
        assert!(!is_exempt_source(Path::new("no_placeholders.rs")));
    }
}
