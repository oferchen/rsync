//! Error types for directory traversal.

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Error encountered during directory traversal.
///
/// Wraps I/O errors with path context for better diagnostics.
/// The error preserves the original I/O error as its source.
///
/// # Examples
///
/// ```no_run
/// use engine::walk::{WalkConfig, WalkdirWalker, WalkError};
/// use std::path::Path;
///
/// let walker = WalkdirWalker::new(Path::new("/nonexistent"), WalkConfig::default());
/// for result in walker {
///     match result {
///         Ok(entry) => println!("{}", entry.path().display()),
///         Err(WalkError::Io { path, .. }) => {
///             eprintln!("I/O error at: {}", path.display());
///         }
///         Err(WalkError::Loop { path, .. }) => {
///             eprintln!("Symlink loop at: {}", path.display());
///         }
///     }
/// }
/// ```
#[derive(Debug, Error)]
pub enum WalkError {
    /// An I/O error occurred while accessing a path.
    #[error("failed to {action} '{path}': {source}", path = path.display())]
    Io {
        /// The action being performed (e.g., "read directory", "stat").
        action: &'static str,
        /// The path where the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// A symbolic link loop was detected.
    #[error("symbolic link loop detected at '{path}'", path = path.display())]
    Loop {
        /// The path where the loop was detected.
        path: PathBuf,
        /// The ancestor path that the link points back to.
        ancestor: PathBuf,
    },
}

impl WalkError {
    /// Creates a loop error.
    pub(crate) fn symlink_loop(path: PathBuf, ancestor: PathBuf) -> Self {
        Self::Loop { path, ancestor }
    }

    /// Returns the path where the error occurred.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::Io { path, .. } | Self::Loop { path, .. } => path,
        }
    }

    /// Returns `true` if this is a permission denied error.
    #[must_use]
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied)
    }

    /// Returns `true` if this is a not found error.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }

    /// Returns `true` if this is a loop error.
    #[must_use]
    pub fn is_loop(&self) -> bool {
        matches!(self, Self::Loop { .. })
    }
}

impl From<walkdir::Error> for WalkError {
    fn from(err: walkdir::Error) -> Self {
        let path = err.path().map(|p| p.to_path_buf()).unwrap_or_default();

        if let Some(ancestor) = err.loop_ancestor() {
            return Self::symlink_loop(path, ancestor.to_path_buf());
        }

        if let Some(io_err) = err.into_io_error() {
            Self::Io {
                action: "traverse",
                path,
                source: io_err,
            }
        } else {
            // Fallback for unexpected error types
            Self::Io {
                action: "traverse",
                path,
                source: io::Error::other("unknown walkdir error"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_displays_correctly() {
        let err = WalkError::Io {
            action: "read directory",
            path: PathBuf::from("/test/path"),
            source: io::Error::new(io::ErrorKind::NotFound, "not found"),
        };

        let msg = err.to_string();
        assert!(msg.contains("read directory"));
        assert!(msg.contains("/test/path"));
    }

    #[test]
    fn loop_error_displays_correctly() {
        let err =
            WalkError::symlink_loop(PathBuf::from("/test/link"), PathBuf::from("/test/ancestor"));

        let msg = err.to_string();
        assert!(msg.contains("symbolic link loop"));
        assert!(msg.contains("/test/link"));
    }

    #[test]
    fn path_accessor_returns_correct_path() {
        let path = PathBuf::from("/test/path");
        let err = WalkError::Io {
            action: "stat",
            path: path.clone(),
            source: io::Error::other("err"),
        };

        assert_eq!(err.path(), &path);
    }

    #[test]
    fn permission_denied_detection() {
        let err = WalkError::Io {
            action: "read directory",
            path: PathBuf::from("/root"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        };

        assert!(err.is_permission_denied());
        assert!(!err.is_not_found());
        assert!(!err.is_loop());
    }

    #[test]
    fn not_found_detection() {
        let err = WalkError::Io {
            action: "stat",
            path: PathBuf::from("/missing"),
            source: io::Error::new(io::ErrorKind::NotFound, "not found"),
        };

        assert!(err.is_not_found());
        assert!(!err.is_permission_denied());
        assert!(!err.is_loop());
    }

    #[test]
    fn loop_detection() {
        let err = WalkError::symlink_loop(PathBuf::from("/a"), PathBuf::from("/b"));

        assert!(err.is_loop());
        assert!(!err.is_permission_denied());
        assert!(!err.is_not_found());
    }

    #[test]
    fn error_is_debug() {
        let err = WalkError::Io {
            action: "read directory",
            path: PathBuf::from("/test"),
            source: io::Error::other("test"),
        };
        let debug = format!("{err:?}");
        assert!(debug.contains("Io"));
    }

    #[test]
    fn error_has_source() {
        use std::error::Error;

        let err = WalkError::Io {
            action: "read directory",
            path: PathBuf::from("/test"),
            source: io::Error::other("inner"),
        };

        assert!(err.source().is_some());
    }
}
