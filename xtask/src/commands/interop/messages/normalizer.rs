//! Message normalization for comparison.
//!
//! Normalizes rsync messages to allow comparison between upstream rsync and oc-rsync.
//! The main difference is that oc-rsync adds Rust source location suffixes like:
//! "... at crates/core/src/message.rs:123 [sender=3.4.1-rust]"
//!
//! We normalize these differences while preserving the essential message content.

use super::extractor::Message;
use regex::Regex;
use std::sync::OnceLock;

/// Normalized message for comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedMessage {
    /// The normalized message text.
    pub text: String,
    /// The role trailer if present.
    pub role: Option<String>,
}

impl NormalizedMessage {
    /// Create a normalized message from a raw message.
    pub fn from_message(message: &Message) -> Self {
        let text = normalize_text(&message.text);
        Self {
            text,
            role: message.role.clone(),
        }
    }

    /// Check if this message matches another (ignoring acceptable variations).
    pub fn matches(&self, other: &NormalizedMessage) -> bool {
        self.text == other.text && self.role == other.role
    }
}

/// Normalize message text by removing variations we expect.
fn normalize_text(text: &str) -> String {
    let mut normalized = text.to_owned();

    // 1. Strip Rust source suffix: " at path/to/file.rs:123"
    normalized = strip_rust_source_suffix(&normalized);

    // 2. Strip version suffixes from role trailers: [sender=3.4.1-rust] -> [sender]
    normalized = strip_version_from_role(&normalized);

    // 3. Normalize absolute paths to relative (if any)
    normalized = normalize_paths(&normalized);

    // 4. Normalize whitespace (collapse multiple spaces, trim)
    normalized = normalize_whitespace(&normalized);

    normalized
}

/// Strip Rust source location suffix like " at crates/core/src/message.rs:123".
fn strip_rust_source_suffix(text: &str) -> String {
    static RUST_SOURCE_RE: OnceLock<Regex> = OnceLock::new();
    let re = RUST_SOURCE_RE
        .get_or_init(|| Regex::new(r"\s+at\s+[\w/\-_.]+\.rs:\d+").expect("valid regex"));

    re.replace_all(text, "").to_string()
}

/// Strip version information from role trailers: `[sender=3.4.1-rust]` -> `[sender]`.
fn strip_version_from_role(text: &str) -> String {
    static ROLE_VERSION_RE: OnceLock<Regex> = OnceLock::new();
    let re = ROLE_VERSION_RE.get_or_init(|| {
        Regex::new(r"\[(sender|receiver|generator|server|client|daemon)=[^\]]+\]")
            .expect("valid regex")
    });

    re.replace_all(text, "[$1]").to_string()
}

/// Normalize absolute paths to relative or generic forms.
fn normalize_paths(text: &str) -> String {
    static ABS_PATH_RE: OnceLock<Regex> = OnceLock::new();
    let re = ABS_PATH_RE.get_or_init(|| {
        // Match absolute paths like /tmp/... or /home/...
        Regex::new(r"/(?:tmp|home|var)/[^\s:]+").expect("valid regex")
    });

    // Replace absolute paths with a generic placeholder
    re.replace_all(text, "<path>").to_string()
}

/// Normalize whitespace: collapse multiple spaces and trim.
fn normalize_whitespace(text: &str) -> String {
    static WHITESPACE_RE: OnceLock<Regex> = OnceLock::new();
    let re = WHITESPACE_RE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"));

    re.replace_all(text.trim(), " ").to_string()
}

/// Normalize a collection of messages for comparison.
pub fn normalize_messages(messages: &[Message]) -> Vec<NormalizedMessage> {
    messages
        .iter()
        .map(NormalizedMessage::from_message)
        .collect()
}

/// Compare two sets of normalized messages and return differences.
pub fn find_differences(
    actual: &[NormalizedMessage],
    expected: &[NormalizedMessage],
) -> Vec<String> {
    let mut differences = Vec::new();

    // Check for messages in actual but not in expected
    for msg in actual {
        if !expected.iter().any(|e| e.matches(msg)) {
            differences.push(format!("Unexpected message: {}", msg.text));
        }
    }

    // Check for messages in expected but not in actual
    for msg in expected {
        if !actual.iter().any(|a| a.matches(msg)) {
            differences.push(format!("Missing message: {}", msg.text));
        }
    }

    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::interop::messages::extractor::Message;

    #[test]
    fn test_strip_rust_source_suffix() {
        let input = "rsync: error in file IO at crates/core/src/message.rs:123 [sender=3.4.1-rust]";
        let output = strip_rust_source_suffix(input);
        assert_eq!(output, "rsync: error in file IO [sender=3.4.1-rust]");
    }

    #[test]
    fn test_strip_version_from_role() {
        let input = "rsync: error [sender=3.4.1-rust]";
        let output = strip_version_from_role(input);
        assert_eq!(output, "rsync: error [sender]");
    }

    #[test]
    fn test_normalize_paths() {
        let input = "rsync: cannot read /tmp/test/file.txt";
        let output = normalize_paths(input);
        assert_eq!(output, "rsync: cannot read <path>");
    }

    #[test]
    fn test_normalize_whitespace() {
        let input = "  rsync:   error   with   spaces  ";
        let output = normalize_whitespace(input);
        assert_eq!(output, "rsync: error with spaces");
    }

    #[test]
    fn test_normalize_message_full() {
        let msg = Message::new(
            "rsync: error in file IO at crates/core/src/message.rs:123 [sender=3.4.1-rust]".to_owned(),
        );
        let normalized = NormalizedMessage::from_message(&msg);
        assert_eq!(normalized.text, "rsync: error in file IO [sender]");
        assert_eq!(normalized.role, Some("sender".to_owned()));
    }

    #[test]
    fn test_message_matches() {
        let msg1 = NormalizedMessage {
            text: "rsync: error [sender]".to_owned(),
            role: Some("sender".to_owned()),
        };
        let msg2 = NormalizedMessage {
            text: "rsync: error [sender]".to_owned(),
            role: Some("sender".to_owned()),
        };
        assert!(msg1.matches(&msg2));
    }
}
