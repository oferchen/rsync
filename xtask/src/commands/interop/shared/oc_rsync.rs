//! oc-rsync binary detection and management for interop testing.

use crate::error::{TaskError, TaskResult};
use std::path::{Path, PathBuf};

/// Detect the oc-rsync binary in the workspace.
pub fn detect_oc_rsync_binary(workspace: &Path) -> TaskResult<OcRsyncBinary> {
    // Try release build first, then debug
    let release_path = workspace.join("target/release/oc-rsync");
    let debug_path = workspace.join("target/debug/oc-rsync");

    let path = if release_path.exists() {
        release_path
    } else if debug_path.exists() {
        debug_path
    } else {
        return Err(TaskError::ToolMissing(
            "oc-rsync binary not found. Run 'cargo build --release' first.".to_string(),
        ));
    };

    Ok(OcRsyncBinary { path })
}

/// Information about the oc-rsync binary.
#[derive(Debug, Clone)]
pub struct OcRsyncBinary {
    /// Path to the binary.
    pub path: PathBuf,
}

impl OcRsyncBinary {
    /// Check if this binary exists and is executable.
    pub fn is_available(&self) -> bool {
        self.path.exists() && self.path.is_file()
    }

    /// Get path to the binary.
    pub fn binary_path(&self) -> &Path {
        &self.path
    }

    /// Get a display name for this binary.
    pub fn name(&self) -> &str {
        "oc-rsync"
    }
}
