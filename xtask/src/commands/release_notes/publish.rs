//! GitHub release creation and update via the `gh` CLI.
//!
//! Creates a new GitHub release or updates an existing one using the
//! rendered release body file and a tag name.

use crate::error::{TaskError, TaskResult};
use crate::util::ensure_command_available;
use std::path::Path;
use std::process::Command;

/// Options for the `publish` subcommand.
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Git tag for the release (e.g., `v0.7.0`).
    pub tag: String,
    /// Path to the rendered release body file.
    pub body_file: std::path::PathBuf,
    /// Create the release as a draft.
    pub draft: bool,
}

/// Creates or updates a GitHub release for the given tag.
pub fn execute(workspace: &Path, options: PublishOptions) -> TaskResult<()> {
    ensure_command_available("gh", "install the GitHub CLI from https://cli.github.com/")?;

    let body_path = if options.body_file.is_absolute() {
        options.body_file.clone()
    } else {
        workspace.join(&options.body_file)
    };

    if !body_path.exists() {
        return Err(TaskError::Validation(format!(
            "release body file not found: {}",
            body_path.display(),
        )));
    }

    let tag = &options.tag;
    let title = tag.clone();

    if release_exists(workspace, tag)? {
        update_release(workspace, tag, &title, &body_path, options.draft)?;
        println!("Updated GitHub release {tag}.");
    } else {
        create_release(workspace, tag, &title, &body_path, options.draft)?;
        println!("Created GitHub release {tag}.");
    }

    Ok(())
}

/// Checks whether a release already exists for the given tag.
fn release_exists(workspace: &Path, tag: &str) -> TaskResult<bool> {
    let output = Command::new("gh")
        .args(["release", "view", tag])
        .current_dir(workspace)
        .output()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to run gh release view: {e}"),
            ))
        })?;

    Ok(output.status.success())
}

/// Creates a new GitHub release.
fn create_release(
    workspace: &Path,
    tag: &str,
    title: &str,
    body_file: &Path,
    draft: bool,
) -> TaskResult<()> {
    let mut args = vec![
        "release".to_owned(),
        "create".to_owned(),
        tag.to_owned(),
        "--title".to_owned(),
        title.to_owned(),
        "--notes-file".to_owned(),
        body_file.display().to_string(),
    ];

    if draft {
        args.push("--draft".to_owned());
    }

    run_gh(workspace, &args)
}

/// Updates an existing GitHub release.
fn update_release(
    workspace: &Path,
    tag: &str,
    title: &str,
    body_file: &Path,
    draft: bool,
) -> TaskResult<()> {
    let mut args = vec![
        "release".to_owned(),
        "edit".to_owned(),
        tag.to_owned(),
        "--title".to_owned(),
        title.to_owned(),
        "--notes-file".to_owned(),
        body_file.display().to_string(),
    ];

    if draft {
        args.push("--draft".to_owned());
    }

    run_gh(workspace, &args)
}

/// Runs a `gh` command with the provided arguments.
fn run_gh(workspace: &Path, args: &[String]) -> TaskResult<()> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|e| {
            TaskError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to run gh: {e}"),
            ))
        })?;

    if output.status.success() {
        return Ok(());
    }

    let _stderr = String::from_utf8_lossy(&output.stderr);
    Err(TaskError::CommandFailed {
        program: format!("gh {}", args.join(" ")),
        status: output.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn publish_rejects_missing_body_file() {
        let dir = tempdir().unwrap();
        let options = PublishOptions {
            tag: "v0.7.0".to_owned(),
            body_file: dir.path().join("nonexistent.md"),
            draft: false,
        };

        let err = execute(dir.path(), options).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn publish_resolves_relative_body_path() {
        let dir = tempdir().unwrap();
        let body = dir.path().join("RELEASE.md");
        fs::write(&body, "## Release\n").unwrap();

        let options = PublishOptions {
            tag: "v0.7.0".to_owned(),
            body_file: std::path::PathBuf::from("RELEASE.md"),
            draft: true,
        };

        // This will fail at the `gh` command (not installed in test env),
        // but we verify the path resolution succeeded by checking the error
        // is not about a missing file.
        let err = execute(dir.path(), options).unwrap_err();
        let message = err.to_string();
        assert!(
            !message.contains("not found"),
            "relative path should resolve against workspace: {message}",
        );
    }
}
