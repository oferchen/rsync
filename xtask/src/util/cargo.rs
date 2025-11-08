use super::env::map_command_error;
use crate::error::{TaskError, TaskResult};
use serde_json::Value as JsonValue;
use std::path::Path;
use std::process::Command;

/// Reads JSON metadata from `cargo metadata`.
pub fn cargo_metadata_json(workspace: &Path) -> TaskResult<JsonValue> {
    let output = Command::new("cargo")
        .current_dir(workspace)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| {
            map_command_error(
                error,
                "cargo metadata",
                "ensure cargo is installed and available in PATH",
            )
        })?;

    if !output.status.success() {
        return Err(TaskError::CommandFailed {
            program: String::from("cargo metadata"),
            status: output.status,
        });
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        TaskError::Metadata(format!("failed to parse cargo metadata JSON: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::cargo_metadata_json;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use tempfile::tempdir;

    fn workspace_root() -> &'static Path {
        static ROOT: OnceLock<PathBuf> = OnceLock::new();
        ROOT.get_or_init(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_path_buf()
        })
    }

    #[test]
    fn cargo_metadata_json_loads_workspace_metadata() {
        let metadata = cargo_metadata_json(workspace_root()).expect("metadata loads");
        assert!(metadata.get("packages").is_some());
    }

    #[test]
    fn cargo_metadata_json_reports_failure() {
        let dir = tempdir().expect("create temp dir");
        let err = cargo_metadata_json(dir.path()).unwrap_err();
        assert!(matches!(
            err,
            crate::error::TaskError::CommandFailed { program, .. } if program == "cargo metadata"
        ));
    }
}
