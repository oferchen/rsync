#![deny(unsafe_code)]

//! README version consistency check command.
//!
//! The `readme-version` command validates that the workspace `README.md`
//! mentions both the upstream rsync release targeted by this build and the
//! Rust-branded version string advertised by the shipping binaries. Keeping the
//! documentation in lock-step with the binaries avoids stale marketing
//! materials and ensures downstream packagers can rely on the README as the
//! canonical human-readable reference for the supported release line.

use crate::error::{TaskError, TaskResult};
use crate::util::read_file_with_context;
use crate::workspace::load_workspace_branding;
use std::path::Path;

/// Executes the `readme-version` command.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let branding = load_workspace_branding(workspace)?;
    let readme_path = workspace.join("README.md");
    let readme = read_file_with_context(&readme_path)?;

    validate_contains(&readme, &branding.rust_version, "Rust-branded version")?;
    validate_contains(&readme, &branding.upstream_version, "upstream version")?;

    println!(
        "README.md references versions '{}' and '{}'",
        branding.rust_version, branding.upstream_version
    );

    Ok(())
}

fn validate_contains(readme: &str, needle: &str, label: &str) -> TaskResult<()> {
    if readme.contains(needle) {
        Ok(())
    } else {
        Err(TaskError::Validation(format!(
            "README.md is missing {label} '{needle}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validate_contains_accepts_present_version() {
        // Supports both version schemes:
        // - Semantic: 0.5.0
        // - Legacy branded: 3.4.1-rust
        validate_contains("rsync 0.5.0", "0.5.0", "rust version")
            .expect("validation succeeds for semantic version");
        validate_contains("rsync 3.4.1-rust", "3.4.1-rust", "rust version")
            .expect("validation succeeds for legacy branded version");
    }

    #[test]
    fn validate_contains_rejects_missing_version() {
        let error = validate_contains("rsync", "3.4.1", "upstream version").unwrap_err();
        assert!(matches!(error, TaskError::Validation(message) if message.contains("README.md")));
    }

    #[test]
    fn execute_reports_versions_present() {
        let workspace = workspace::workspace_root().expect("resolve workspace root");
        execute(&workspace).expect("workspace README matches versions");
    }

    mod workspace {
        use super::*;

        pub fn workspace_root() -> TaskResult<PathBuf> {
            crate::workspace::workspace_root()
        }
    }
}
