/// Represents a planned action during a dry run.
///
/// Each variant represents an operation that would be performed if not running
/// in dry-run mode. Mirrors upstream rsync's `--dry-run` (-n) action semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRunAction {
    /// Would send this file to the remote.
    SendFile {
        /// Path relative to destination.
        path: String,
        /// File size in bytes.
        size: u64,
    },
    /// Would receive this file from the remote.
    ReceiveFile {
        /// Path relative to destination.
        path: String,
        /// File size in bytes.
        size: u64,
    },
    /// Would delete this file.
    DeleteFile {
        /// Path relative to destination.
        path: String,
    },
    /// Would delete this directory.
    DeleteDir {
        /// Path relative to destination (ends with `/`).
        path: String,
    },
    /// Would create this directory.
    CreateDir {
        /// Path relative to destination (ends with `/`).
        path: String,
    },
    /// Would update permissions on this file/directory.
    UpdatePerms {
        /// Path relative to destination.
        path: String,
    },
    /// Would create this symlink.
    CreateSymlink {
        /// Path relative to destination.
        path: String,
        /// Symlink target.
        target: String,
    },
    /// Would create this hard link.
    CreateHardlink {
        /// Path relative to destination.
        path: String,
        /// Hard link target.
        target: String,
    },
}

impl DryRunAction {
    /// Returns the path associated with this action.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::SendFile { path, .. }
            | Self::ReceiveFile { path, .. }
            | Self::DeleteFile { path }
            | Self::DeleteDir { path }
            | Self::CreateDir { path }
            | Self::UpdatePerms { path }
            | Self::CreateSymlink { path, .. }
            | Self::CreateHardlink { path, .. } => path,
        }
    }

    /// Returns the size associated with this action, if any.
    pub fn size(&self) -> Option<u64> {
        match self {
            Self::SendFile { size, .. } | Self::ReceiveFile { size, .. } => Some(*size),
            _ => None,
        }
    }

    /// Returns `true` if this action is a deletion.
    #[must_use]
    pub const fn is_deletion(&self) -> bool {
        matches!(self, Self::DeleteFile { .. } | Self::DeleteDir { .. })
    }

    /// Returns `true` if this action is a directory operation.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        matches!(self, Self::CreateDir { .. } | Self::DeleteDir { .. })
    }
}
