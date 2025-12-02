//! Common utilities for interop testing.

use std::path::PathBuf;

/// Get the path to the oc-rsync binary.
#[allow(dead_code)]
pub fn get_oc_rsync_binary(workspace: &std::path::Path) -> PathBuf {
    // Try dist profile first (used in CI)
    let dist_path = workspace.join("target/dist/oc-rsync");
    if dist_path.exists() {
        return dist_path;
    }

    // Fall back to release profile
    let release_path = workspace.join("target/release/oc-rsync");
    if release_path.exists() {
        return release_path;
    }

    // Fall back to debug profile
    workspace.join("target/debug/oc-rsync")
}
