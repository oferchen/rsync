use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Error produced when metadata preservation fails.
#[derive(Debug)]
pub struct MetadataError {
    context: &'static str,
    path: PathBuf,
    source: io::Error,
}

impl MetadataError {
    /// Creates a new [`MetadataError`] from the supplied context, path, and source error.
    pub(crate) fn new(context: &'static str, path: &Path, source: io::Error) -> Self {
        Self {
            context,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Returns the operation being performed when the error occurred.
    #[must_use]
    pub const fn context(&self) -> &'static str {
        self.context
    }

    /// Returns the path involved in the failing operation.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the underlying [`io::Error`] that triggered this failure.
    #[must_use]
    pub fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Consumes the error and returns its constituent parts.
    #[must_use]
    pub fn into_parts(self) -> (&'static str, PathBuf, io::Error) {
        (self.context, self.path, self.source)
    }
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to {} '{}': {}",
            self.context,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
