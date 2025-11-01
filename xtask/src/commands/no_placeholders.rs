use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, list_rust_sources_via_git, validation_error};
use std::ffi::OsString;
use std::fs;
use std::io::BufRead;
use std::path::Path;

#[cfg(test)]
use std::path::PathBuf;

/// Options accepted by the `no-placeholders` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoPlaceholdersOptions;

const TODO_MACRO_BYTES: [u8; 5] = [b't', b'o', b'd', b'o', b'!'];
const UNIMPLEMENTED_MACRO_BYTES: [u8; 14] = [
    b'u', b'n', b'i', b'm', b'p', b'l', b'e', b'm', b'e', b'n', b't', b'e', b'd', b'!',
];
const PANIC_MACRO_BYTES: [u8; 6] = [b'p', b'a', b'n', b'i', b'c', b'!'];
const TODO_WORD_BYTES: [u8; 4] = [b't', b'o', b'd', b'o'];
const FIXME_WORD_BYTES: [u8; 5] = [b'f', b'i', b'x', b'm', b'e'];
const TRIPLE_X_WORD_BYTES: [u8; 3] = [b'x', b'x', b'x'];
const UNIMPLEMENTED_WORD_BYTES: [u8; 13] = [
    b'u', b'n', b'i', b'm', b'p', b'l', b'e', b'm', b'e', b'n', b't', b'e', b'd',
];

/// Parses CLI arguments for the `no-placeholders` command.
pub fn parse_args<I>(args: I) -> TaskResult<NoPlaceholdersOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();

    if let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        return Err(TaskError::Usage(format!(
            "unrecognised argument '{}' for no-placeholders command",
            arg.to_string_lossy()
        )));
    }

    Ok(NoPlaceholdersOptions)
}

/// Executes the `no-placeholders` command.
pub fn execute(workspace: &Path, _options: NoPlaceholdersOptions) -> TaskResult<()> {
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
            "placeholder markers detected in Rust sources; remove to-do!/un-implemented! markers, ",
            "fix-me notes, and triple-x references"
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
    let mut panic_tracker = PanicTracker::new();

    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }

        line_number += 1;
        if line_number == 1 {
            continue;
        }

        let line = buffer.trim_end_matches(['\r', '\n']);
        let panic_index = find_subsequence(line.as_bytes(), &PANIC_MACRO_BYTES);
        let panic_context = panic_tracker.is_active() || panic_index.is_some();
        if contains_placeholder(line, panic_context) {
            findings.push(PlaceholderFinding {
                line: line_number,
                snippet: line.to_string(),
            });
        }

        if let Some(index) = panic_index {
            panic_tracker.consume(&line[..index]);
            panic_tracker.start(&line[index + PANIC_MACRO_BYTES.len()..]);
        } else {
            panic_tracker.consume(line);
        }
    }

    Ok(findings)
}

fn contains_placeholder(line: &str, panic_context: bool) -> bool {
    let line_bytes = line.as_bytes();
    if contains_subsequence(line_bytes, &TODO_MACRO_BYTES)
        || contains_subsequence(line_bytes, &UNIMPLEMENTED_MACRO_BYTES)
    {
        return true;
    }

    let mut lower_bytes = line_bytes.to_vec();
    lower_bytes.make_ascii_lowercase();

    if contains_standalone_sequence(&lower_bytes, &FIXME_WORD_BYTES)
        || contains_standalone_sequence(&lower_bytes, &TRIPLE_X_WORD_BYTES)
    {
        return true;
    }

    if panic_context
        && (contains_standalone_sequence(&lower_bytes, &TODO_WORD_BYTES)
            || contains_standalone_sequence(&lower_bytes, &FIXME_WORD_BYTES)
            || contains_standalone_sequence(&lower_bytes, &TRIPLE_X_WORD_BYTES)
            || contains_standalone_sequence(&lower_bytes, &UNIMPLEMENTED_WORD_BYTES))
    {
        return true;
    }

    false
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanicDelimiter {
    Parenthesis,
    Brace,
    Bracket,
}

impl PanicDelimiter {
    fn from_char(ch: char) -> Option<Self> {
        match ch {
            '(' => Some(Self::Parenthesis),
            '{' => Some(Self::Brace),
            '[' => Some(Self::Bracket),
            _ => None,
        }
    }

    fn delta(self, text: &str) -> i32 {
        let (open, close) = match self {
            Self::Parenthesis => ('(', ')'),
            Self::Brace => ('{', '}'),
            Self::Bracket => ('[', ']'),
        };

        let mut count = 0i32;
        for ch in text.chars() {
            if ch == open {
                count += 1;
            } else if ch == close {
                count -= 1;
            }
        }

        count
    }
}

#[derive(Debug, Default)]
struct PanicTracker {
    state: PanicState,
}

#[derive(Debug, Eq, PartialEq)]
enum PanicState {
    Inactive,
    AwaitingDelimiter {
        block_comment_depth: u32,
    },
    Active {
        delimiter: PanicDelimiter,
        depth: i32,
    },
}

impl Default for PanicState {
    fn default() -> Self {
        PanicState::Inactive
    }
}

impl PanicTracker {
    fn new() -> Self {
        Self::default()
    }

    fn is_active(&self) -> bool {
        !matches!(self.state, PanicState::Inactive)
    }

    fn reset(&mut self) {
        self.state = PanicState::Inactive;
    }

    fn start(&mut self, after_macro: &str) {
        self.state = PanicState::AwaitingDelimiter {
            block_comment_depth: 0,
        };
        self.consume(after_macro);
    }

    fn consume(&mut self, segment: &str) {
        match &mut self.state {
            PanicState::Inactive => {}
            PanicState::AwaitingDelimiter {
                block_comment_depth,
            } => {
                let mut depth = *block_comment_depth;
                let bytes = segment.as_bytes();
                let mut index = 0usize;
                while index < bytes.len() {
                    if depth > 0 {
                        if index + 1 < bytes.len()
                            && bytes[index] == b'*'
                            && bytes[index + 1] == b'/'
                        {
                            depth -= 1;
                            index += 2;
                            continue;
                        }

                        index += 1;
                        continue;
                    }

                    match bytes[index] {
                        b' ' | b'\t' | b'\r' | b'\n' => {
                            index += 1;
                        }
                        b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'/' => {
                            *block_comment_depth = depth;
                            return;
                        }
                        b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                            depth += 1;
                            index += 2;
                        }
                        b'(' | b'{' | b'[' => {
                            let opening = bytes[index] as char;
                            let Some(delimiter) = PanicDelimiter::from_char(opening) else {
                                self.reset();
                                return;
                            };
                            let rest = &segment[index + 1..];
                            let paren_depth = 1 + delimiter.delta(rest);
                            if paren_depth <= 0 {
                                self.reset();
                            } else {
                                self.state = PanicState::Active {
                                    delimiter,
                                    depth: paren_depth,
                                };
                            }

                            return;
                        }
                        _ => {
                            self.reset();
                            return;
                        }
                    }
                }

                *block_comment_depth = depth;
            }
            PanicState::Active { delimiter, depth } => {
                let delta = delimiter.delta(segment);
                if delta == 0 {
                    return;
                }

                let new_depth = *depth + delta;
                if new_depth <= 0 {
                    self.reset();
                } else {
                    *depth = new_depth;
                }
            }
        }
    }
}

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask no-placeholders\n\nOptions:\n  -h, --help      Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(suffix: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("oc_rsync_xtask_{now}_{suffix}"))
    }

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, NoPlaceholdersOptions);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("no-placeholders")));
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
    fn scan_ignores_first_line() {
        let path = unique_temp_path("first_line_ignored");
        let note = ["TO", "DO"].concat();
        let content = format!("// {note}: license\nfn ok() {{}}\n");
        fs::write(&path, content).expect("write sample");
        let findings = scan_rust_file_for_placeholders(&path).expect("scan succeeds");
        fs::remove_file(&path).expect("cleanup sample");
        assert!(findings.is_empty());
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
