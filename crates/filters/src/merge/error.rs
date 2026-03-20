//! Error type for merge file operations.

use std::io;
use std::path::Path;

/// Error produced when a merge file cannot be read or contains invalid syntax.
///
/// The error carries the source file path and, when available, the 1-indexed
/// line number where the problem was detected.  The [`Display`](std::fmt::Display)
/// implementation formats this as `path:line: message` (or `path: message` when
/// no line number applies).
#[derive(Debug)]
pub struct MergeFileError {
    /// The file path that caused the error.
    pub path: String,
    /// The line number (1-indexed) if applicable.
    pub line: Option<usize>,
    /// Human-readable description of the error.
    pub message: String,
}

impl std::fmt::Display for MergeFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{}: {}", self.path, line, self.message),
            None => write!(f, "{}: {}", self.path, self.message),
        }
    }
}

impl std::error::Error for MergeFileError {}

impl MergeFileError {
    pub(crate) fn io_error(path: &Path, error: &io::Error) -> Self {
        Self {
            path: path.display().to_string(),
            line: None,
            message: error.to_string(),
        }
    }

    pub(crate) fn parse_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            path: path.display().to_string(),
            line: Some(line),
            message: message.into(),
        }
    }
}
