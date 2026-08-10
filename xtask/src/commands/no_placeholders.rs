use crate::error::TaskResult;
use crate::util::{list_rust_sources_via_git, validation_error};
use std::fs;
use std::io::BufRead;
use std::path::Path;

const TODO_MACRO_BYTES: [u8; 5] = [b't', b'o', b'd', b'o', b'!'];
const UNIMPLEMENTED_MACRO_BYTES: [u8; 14] = [
    b'u', b'n', b'i', b'm', b'p', b'l', b'e', b'm', b'e', b'n', b't', b'e', b'd', b'!',
];
const TODO_WORD_BYTES: [u8; 4] = [b't', b'o', b'd', b'o'];
const FIXME_WORD_BYTES: [u8; 5] = [b'f', b'i', b'x', b'm', b'e'];
const TRIPLE_X_WORD_BYTES: [u8; 3] = [b'x', b'x', b'x'];
const UNIMPLEMENTED_WORD_BYTES: [u8; 13] = [
    b'u', b'n', b'i', b'm', b'p', b'l', b'e', b'm', b'e', b'n', b't', b'e', b'd',
];

/// Executes the `no-placeholders` command.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let mut violations_present = false;
    let rust_files = list_rust_sources_via_git(workspace)?;

    for relative in rust_files {
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
            "placeholder markers detected in Rust sources; remove todo/unimplemented markers, ",
            "fixme notes, and triple-x references"
        )));
    }

    Ok(())
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
    if contains_subsequence(line_bytes, &TODO_MACRO_BYTES)
        || contains_subsequence(line_bytes, &UNIMPLEMENTED_MACRO_BYTES)
    {
        return true;
    }

    let mut lower_bytes = line_bytes.to_vec();
    lower_bytes.make_ascii_lowercase();

    contains_standalone_sequence(&lower_bytes, &TODO_WORD_BYTES)
        || contains_standalone_sequence(&lower_bytes, &UNIMPLEMENTED_WORD_BYTES)
        || contains_standalone_sequence(&lower_bytes, &FIXME_WORD_BYTES)
        || contains_standalone_sequence(&lower_bytes, &TRIPLE_X_WORD_BYTES)
}

fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }

    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn contains_standalone_sequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }

    let mut index = 0usize;
    while index + needle.len() <= haystack.len() {
        if &haystack[index..index + needle.len()] == needle {
            let before_ok = index == 0 || !is_identifier_byte(haystack[index - 1]);
            let after_index = index + needle.len();
            let after_ok =
                after_index == haystack.len() || !is_identifier_byte(haystack[after_index]);

            if before_ok && after_ok {
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

    #[test]
    fn scan_detects_todo_macro() {
        let path = unique_temp_path("todo_macro");
        let macro_name = ["to", "do!"].concat();
        let content = format!("fn example() {{\n    {macro_name}();\n}}\n");
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].snippet.contains(&macro_name));
    }

    #[test]
    fn scan_detects_fixme_comment() {
        let path = unique_temp_path("fixme_comment");
        let marker = ["FIX", "ME"].concat();
        let content = format!("// header\n// {marker}: implement\nfn ready() {{}}\n");
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
        let marker_lower = marker.to_ascii_lowercase();
        assert!(
            findings[0]
                .snippet
                .to_ascii_lowercase()
                .contains(&marker_lower)
        );
    }

    #[test]
    fn scan_detects_todo_comment() {
        let path = unique_temp_path("todo_comment");
        let content = "// TODO: fill in implementation\nfn stub() {}\n";
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 1);
        assert!(findings[0].snippet.to_ascii_lowercase().contains("todo"));
    }

    #[test]
    fn scan_detects_first_line_placeholder() {
        let path = unique_temp_path("first_line_placeholder");
        let marker = ["FIX", "ME"].concat();
        let content = format!("// {marker}: license\nfn ok() {{}}\n");
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 1);
        let marker_lower = marker.to_ascii_lowercase();
        assert!(
            findings[0]
                .snippet
                .to_ascii_lowercase()
                .contains(&marker_lower)
        );
    }

    #[test]
    fn scan_detects_placeholder_inside_multiline_panic() {
        let path = unique_temp_path("panic_multiline");
        let todo = ["TO", "DO"].concat();
        let content =
            format!("fn explode() {{\n    panic!(\n        \"{todo}: revisit\"\n    );\n}}\n");
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
        let snippet_lower = findings[0].snippet.to_ascii_lowercase();
        assert!(snippet_lower.contains(&todo.to_ascii_lowercase()));
    }
}
