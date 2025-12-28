use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Error produced when metadata preservation fails.
#[derive(Debug, Error)]
#[error("failed to {context} '{}': {source}", path.display())]
pub struct MetadataError {
    context: &'static str,
    path: PathBuf,
    #[source]
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
    pub const fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Consumes the error and returns its constituent parts.
    #[must_use]
    pub fn into_parts(self) -> (&'static str, PathBuf, io::Error) {
        (self.context, self.path, self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataError;
    use std::error::Error as _;
    use std::io;
    use std::path::Path;

    #[test]
    fn metadata_error_exposes_contextual_information() {
        let source = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let error = MetadataError::new("set xattr", Path::new("/tmp/file"), source);

        assert_eq!(error.context(), "set xattr");
        assert_eq!(error.path(), Path::new("/tmp/file"));
        assert_eq!(error.source_error().kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("set xattr"));
        assert!(error.source().is_some());

        let (context, path, inner) = error.into_parts();
        assert_eq!(context, "set xattr");
        assert_eq!(path, Path::new("/tmp/file"));
        assert_eq!(inner.kind(), io::ErrorKind::PermissionDenied);
    }
}
