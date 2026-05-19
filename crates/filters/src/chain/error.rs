//! Errors produced by per-directory filter chain operations.

use std::io;
use std::path::PathBuf;

use crate::FilterError;

/// Error produced during per-directory filter chain operations.
#[derive(Debug)]
pub enum FilterChainError {
    /// A merge file could not be read.
    Io {
        /// Path to the file that caused the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A merge file contained invalid filter syntax.
    Parse {
        /// Path to the file that caused the error.
        path: PathBuf,
        /// Description of the parse error.
        message: String,
    },
    /// A parsed rule could not be compiled into a glob matcher.
    Filter(FilterError),
}

impl std::fmt::Display for FilterChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    f,
                    "failed to read merge file {}: {}",
                    path.display(),
                    source
                )
            }
            Self::Parse { path, message } => {
                write!(
                    f,
                    "failed to parse merge file {}: {}",
                    path.display(),
                    message
                )
            }
            Self::Filter(e) => write!(f, "filter compilation error: {e}"),
        }
    }
}

impl std::error::Error for FilterChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { .. } => None,
            Self::Filter(e) => Some(e),
        }
    }
}
