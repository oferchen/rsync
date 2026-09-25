//! [`MetadataError`] type used by every fallible operation in the crate.
//!
//! Pairs a static context string, the affected path, and the underlying
//! [`io::Error`] so higher layers can render upstream-compatible diagnostics.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Context of a failed ownership change that alters the owner.
pub(crate) const CHOWN_CONTEXT: &str = "preserve ownership";

/// Context of a failed ownership change that alters only the group.
pub(crate) const CHGRP_CONTEXT: &str = "preserve group";

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

    /// Relabels the failing operation, keeping the path and source error.
    #[cfg(unix)]
    pub(crate) fn with_context(mut self, context: &'static str) -> Self {
        self.context = context;
        self
    }

    /// Renders the `rsyserr()` text upstream's `set_file_attrs()` prints for
    /// this failure, given the caller's `full_fname()` rendering of the path.
    ///
    /// Returns `None` for an operation `set_file_attrs()` has no `rsyserr()`
    /// arm for; the caller keeps its own rendering for those.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:682-684` - `"%s %s failed"` with `chown` or `chgrp`
    /// - `rsync.c:781` - `"failed to set times on %s"`
    /// - `rsync.c:811-813` - `"failed to set permissions on %s"`
    /// - `log.c:499-500` - `rsyserr()` appends `": %s (%d)"`
    #[must_use]
    pub fn set_file_attrs_text(&self, full_fname: &str) -> Option<String> {
        let operation = match self.context {
            CHOWN_CONTEXT => format!("chown {full_fname} failed"),
            CHGRP_CONTEXT => format!("chgrp {full_fname} failed"),
            "preserve timestamps" | "preserve access time" => {
                format!("failed to set times on {full_fname}")
            }
            "preserve permissions" => format!("failed to set permissions on {full_fname}"),
            _ => return None,
        };
        Some(format!(
            "{operation}: {}",
            logging::upstream_errno_text(&self.source)
        ))
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

    /// Each `set_file_attrs()` arm keeps upstream's wording, because the peer
    /// renders the line verbatim and operators grep for the upstream text.
    #[cfg(unix)]
    #[test]
    fn set_file_attrs_text_uses_upstream_wording_per_operation() {
        let text = |context| {
            MetadataError::new(context, Path::new("/d/x"), io::Error::from_raw_os_error(1))
                .set_file_attrs_text("\"/d/x\"")
        };
        assert_eq!(
            text(super::CHOWN_CONTEXT).as_deref(),
            Some("chown \"/d/x\" failed: Operation not permitted (1)")
        );
        assert_eq!(
            text(super::CHGRP_CONTEXT).as_deref(),
            Some("chgrp \"/d/x\" failed: Operation not permitted (1)")
        );
        assert_eq!(
            text("preserve timestamps").as_deref(),
            Some("failed to set times on \"/d/x\": Operation not permitted (1)")
        );
        assert_eq!(
            text("preserve permissions").as_deref(),
            Some("failed to set permissions on \"/d/x\": Operation not permitted (1)")
        );
        assert_eq!(text("set xattr"), None);
    }
}
