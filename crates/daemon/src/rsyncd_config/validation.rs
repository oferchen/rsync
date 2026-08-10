//! Error types for rsyncd configuration parsing and validation.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Error returned when parsing or validating an `rsyncd.conf` file fails.
///
/// Carries the file path and line number (when available) so callers can
/// produce precise diagnostics.
#[derive(Debug, Clone)]
pub struct ConfigError {
    #[allow(dead_code)] // REASON: stored for future diagnostic output
    kind: ErrorKind,
    line: Option<usize>,
    message: String,
    path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // REASON: variants constructed but not yet matched on; for future diagnostics
pub(crate) enum ErrorKind {
    Io,
    Parse,
    Validation,
}

impl ConfigError {
    pub(crate) fn io_error(path: &Path, source: io::Error) -> Self {
        Self {
            kind: ErrorKind::Io,
            line: None,
            message: format!("failed to read '{}': {}", path.display(), source),
            path: Some(path.to_path_buf()),
        }
    }

    pub(crate) fn parse_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Parse,
            line: Some(line),
            message: message.into(),
            path: Some(path.to_path_buf()),
        }
    }

    pub(crate) fn validation_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Validation,
            line: Some(line),
            message: message.into(),
            path: Some(path.to_path_buf()),
        }
    }

    /// Returns the line number where the error occurred, if available.
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// Returns the configuration file path where the error occurred.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(f, "{}: ", path.display())?;
        }
        if let Some(line) = self.line {
            write!(f, "line {line}: ")?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ConfigError {}
