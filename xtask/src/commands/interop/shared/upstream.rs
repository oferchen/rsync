//! Upstream rsync binary detection and management.

use crate::error::{TaskError, TaskResult};
use std::path::{Path, PathBuf};

/// Supported upstream rsync versions for interop testing.
pub const UPSTREAM_VERSIONS: &[&str] = &["3.0.9", "3.1.3", "3.4.1"];

/// Detect available upstream rsync binaries.
pub fn detect_upstream_binaries(workspace: &Path) -> TaskResult<Vec<UpstreamBinary>> {
    let interop_dir = workspace.join("target/interop/upstream-install");

    let mut binaries = Vec::new();

    for version in UPSTREAM_VERSIONS {
        let binary_path = interop_dir.join(version).join("bin/rsync");

        if binary_path.exists() {
            binaries.push(UpstreamBinary {
                version: (*version).to_owned(),
                path: binary_path,
            });
        }
    }

    if binaries.is_empty() {
        return Err(TaskError::ToolMissing(format!(
            "No upstream rsync binaries found in {}\n\
             Run 'bash tools/ci/run_interop.sh' first to build upstream versions.",
            interop_dir.display()
        )));
    }

    Ok(binaries)
}

/// Information about an upstream rsync binary.
#[derive(Debug, Clone)]
pub struct UpstreamBinary {
    /// Version string (e.g., "3.4.1").
    pub version: String,
    /// Path to the binary.
    pub path: PathBuf,
}

impl UpstreamBinary {
    /// Check if this binary exists and is executable.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        self.path.exists() && self.path.is_file()
    }

    /// Get version string for display.
    #[allow(dead_code)]
    pub fn version_string(&self) -> &str {
        &self.version
    }

    /// Get path to the binary.
    pub fn binary_path(&self) -> &Path {
        &self.path
    }
}
